# Leptos View Transitions Spike — Design

**Date:** 2026-05-17
**Branch:** `spike/leptos-view-transitions` (off `agicash-rs/feat/leptos-home-page`)
**Status:** Spike — not production-grade

## Purpose

Prove that the browser-native View Transitions API can drive route
animations inside `leptos_router` (Leptos 0.7) so the agicash PWA can
match the iOS-native animated navigation feel that the archived React
app already provides.

## Phase 1 Findings — React Side

The archived React app (`agicash-rs/archive/react-web-app`, tag
`react-web-app-final`) uses the View Transitions API directly. No
framer-motion, no GSAP. Pieces:

1. **`app/lib/transitions/transitions.css`** — 8 keyframes
   (`slide-in/out-from-{left,right,top,bottom}`, `fade-in`, `fade-out`)
   plus `::view-transition-old(root)` / `::view-transition-new(root)`
   rules that read `--direction-out` / `--direction-in` CSS variables
   off `:root`. Duration 0.18s ease-in.
2. **`app/lib/transitions/view-transition.tsx`** — `LinkWithViewTransition`
   sets the CSS vars synchronously on click (so prefetched routes that
   skip the loading state still animate), passes
   `viewTransition: true` to React Router 7's `<Link>` (RR then calls
   `document.startViewTransition()`), and stores
   `{ transition, applyTo }` in `history.state` so the
   `useViewTransitionEffect` hook can replay/reverse it on
   browser-back navigations.
3. **Named (shared-element) transitions** — `viewTransitionName: 'x'`
   set conditionally on DOM nodes during the transition window using
   `useViewTransitionState(to)` (e.g.
   `app/features/gift-cards/discover-gift-cards.tsx:42`). The
   gift-cards feature is the only consumer (gift card morphs to detail
   view, available-cards section fades together).
4. **Per-feature CSS** —
   `app/features/gift-cards/transitions.css` registers extra
   `::view-transition-{old,new}(name)` rules for the named transitions.

The transition vocabulary used across routes:

| Transition | Where |
|---|---|
| `slideLeft` (header → settings, → transactions) | home `_protected._index.tsx` |
| `slideRight` (back, or to gift-cards) | home → gift-cards |
| `slideUp` (modal-like sheet) | home → receive, send, buy, scan |
| `slideDown` (close sheet) | typically the back-arrow on modal pages |
| `fade` | default cross-route, used by accept-terms etc. |
| Named (shared element) | gift-cards only |

Browser support: View Transitions API is shipped in Chrome 111+,
Edge 111+, Safari 18+, and Firefox 134+. Below that the React code's
synchronous CSS-var write is harmless and the navigation just happens
without animation. There is no explicit feature-detect / fallback in
the React code.

## Leptos Spike Scope

Goal: prove the same mechanism works inside `leptos_router` and
identify rough edges. Out-of-scope for the spike: theme integration,
back-button reverse animation, shared-element across-route state plumbing
beyond a simple demo, production polish.

### Files added

| File | Purpose |
|---|---|
| `crates/agicash-web-leptos/src/transitions/mod.rs` | Public `Transition` enum + helpers |
| `crates/agicash-web-leptos/src/transitions/view_transition.rs` | `start_view_transition`, `apply_transition_styles`, `clear_transition_styles` — wasm-gated wrappers around `web_sys` |
| `crates/agicash-web-leptos/src/transitions/animated_link.rs` | `AnimatedA` component — drop-in `<A>` replacement that triggers a view transition on click |
| `crates/agicash-web-leptos/style/transitions.css` | Keyframes + `::view-transition-{old,new}(root)` driven by CSS vars |
| `crates/agicash-web-leptos/src/pages/spike_transitions.rs` | Demo route family `/spike/transitions/{a,b,sheet}` exercising 3 distinct transitions |

### Files modified

| File | Change |
|---|---|
| `crates/agicash-web-leptos/src/lib.rs` | Add `pub mod transitions;` |
| `crates/agicash-web-leptos/src/app.rs` | Register `/spike/transitions/*` routes, link transitions.css |
| `crates/agicash-web-leptos/src/pages/mod.rs` | Export new spike pages |
| `crates/agicash-web-leptos/index.html` | Load `style/transitions.css` |

### Files NOT touched

Per parent instruction: `pages/home.rs`, `pages/receive.rs`,
`pages/receive_cashu.rs`. Sibling lanes own those.

## API Surface

```rust
// crates/agicash-web-leptos/src/transitions/mod.rs

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Fade,
    SlideUp,    // modal-sheet enter
    SlideDown,  // modal-sheet exit
    SlideLeft,  // forward navigation
    SlideRight, // back navigation
}

// view_transition.rs (all #[cfg(target_arch = "wasm32")])
pub fn apply_transition_styles(t: Transition);
pub fn clear_transition_styles();
pub fn start_view_transition<F: FnOnce() + 'static>(cb: F);
// Non-wasm stubs of the same names so the rlib build compiles.

// animated_link.rs
#[component]
pub fn AnimatedA(
    #[prop(into)] href: String,
    transition: Transition,
    children: Children,
) -> impl IntoView;
```

### Mechanism

`AnimatedA` renders a plain `<a href=...>` with `on:click` that:

1. `event.prevent_default()`
2. `apply_transition_styles(transition)` — writes `--vt-direction-in`,
   `--vt-direction-out`, `--vt-z-index-in`, `--vt-z-index-out` on
   `document.documentElement.style`
3. `start_view_transition(move || navigate(href, NavigateOptions::default()))`
   — `web_sys::HtmlDocument::start_view_transition(callback)` if
   available, otherwise just invokes the callback (Firefox <134 etc.)
4. Schedules `clear_transition_styles()` ~220ms later via
   `gloo-timers` so subsequent navigations don't inherit stale vars.

### Demo routes

- `/spike/transitions/a` — colored circle (red, top-left) labelled
  "Page A". Three buttons: → B (slideLeft), → Sheet (slideUp), → A self
  (fade).
- `/spike/transitions/b` — Bigger colored circle (red, centered)
  labelled "Page B". Back button (slideRight to A). The circle has
  `view-transition-name: spike-hero` set on both pages so the browser
  morphs between sizes/positions.
- `/spike/transitions/sheet` — modal-style page with close button
  (slideDown to A).

## Build + verify

```sh
nix develop .#wasm
PREK_ALLOW_NO_CONFIG=1   # for any commits
cd crates/agicash-web-leptos && wasm-pack build --target web --out-dir pkg --dev
python3 -m http.server 3000
# Open http://localhost:3000 → /login → guest → /spike/transitions/a
```

## Open questions / known limitations

- Back-button reverse animation is not implemented in the spike. It
  would need a history-index tracker (React side has one in
  `useViewTransitionEffect` lines 222-236) plus state reading from
  `window.history.state`. Recommend implementing in the production
  port.
- `clear_transition_styles` uses a fixed timeout rather than
  `ViewTransition.finished` (a Promise) — fine for the spike, but the
  production port should `.then()` off that promise.
- The shared-element demo will only work cleanly in Chrome/Edge/Safari
  18+. Firefox <134 will fall back to a cross-fade.

## Recommendation

Pre-decision: the View Transitions API is a 1-2 day port to make
production-grade in Leptos. The spike will validate the mechanism;
the writeup will give a concrete effort estimate.
