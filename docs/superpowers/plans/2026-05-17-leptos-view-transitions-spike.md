# Leptos View Transitions Spike — Implementation Plan

> Spike, not production. Goal: get the View Transitions API firing
> from a leptos_router `<A>` click and capture a screenshot.

**Goal:** Prove `document.startViewTransition()` integrates with
`leptos_router` 0.7 navigation; demonstrate three transition styles
(fade, slide-up sheet, named shared-element) so the team can decide
whether to commit to this approach for the PWA's navigation animation.

**Architecture:** New `transitions` module wraps the
`web_sys::Document::start_view_transition` API behind a Rust
interface; an `AnimatedA` component renders a plain `<a>` and
intercepts the click to write CSS direction vars and call the API
before invoking `use_navigate()`. Demo routes under
`/spike/transitions/*` exercise the patterns visually.

**Tech Stack:** Leptos 0.7, leptos_router 0.7, web-sys, gloo-timers,
plain CSS keyframes.

---

### Task 1 — Add transitions module skeleton

**Files:**
- Create: `crates/agicash-web-leptos/src/transitions/mod.rs`
- Create: `crates/agicash-web-leptos/src/transitions/view_transition.rs`
- Create: `crates/agicash-web-leptos/src/transitions/animated_link.rs`
- Modify: `crates/agicash-web-leptos/src/lib.rs`

- [ ] Step 1.1 — Write `mod.rs` exposing `Transition` enum + re-exports
- [ ] Step 1.2 — Write wasm-gated `view_transition.rs` with
  `apply_transition_styles`, `clear_transition_styles`,
  `start_view_transition` plus native no-op stubs
- [ ] Step 1.3 — Write `animated_link.rs` with `AnimatedA` component
- [ ] Step 1.4 — `pub mod transitions;` in `lib.rs`
- [ ] Step 1.5 — `cargo check --target wasm32-unknown-unknown -p agicash-web-leptos`

### Task 2 — Wire transitions.css

**Files:**
- Create: `crates/agicash-web-leptos/style/transitions.css`
- Modify: `crates/agicash-web-leptos/index.html`

- [ ] Step 2.1 — Write keyframes (slide-{in,out}-from-{left,right,top,bottom}, fade-in, fade-out)
- [ ] Step 2.2 — Write `::view-transition-old(root)` + `::view-transition-new(root)` reading CSS vars
- [ ] Step 2.3 — Add `<link rel="stylesheet" href="style/transitions.css">` in `index.html`

### Task 3 — Spike demo route

**Files:**
- Create: `crates/agicash-web-leptos/src/pages/spike_transitions.rs`
- Modify: `crates/agicash-web-leptos/src/pages/mod.rs`
- Modify: `crates/agicash-web-leptos/src/app.rs`

- [ ] Step 3.1 — Write three page components (SpikeA, SpikeB, SpikeSheet)
- [ ] Step 3.2 — Export from `pages/mod.rs`
- [ ] Step 3.3 — Register routes `/spike/transitions/{a,b,sheet}` in `app.rs`

### Task 4 — Build and smoke-test

- [ ] Step 4.1 — `nix develop .#wasm` then `aweb` → expect a clean wasm-pack build
- [ ] Step 4.2 — `python3 -m http.server 3000` from the crate dir
- [ ] Step 4.3 — Open browser, navigate to `/spike/transitions/a`,
  click through B and Sheet, watch for animations
- [ ] Step 4.4 — Capture screenshot or describe what was seen

### Task 5 — Commit and push

- [ ] Step 5.1 — `git add -A` (new files only — the spike doesn't
  modify existing app code beyond the route registration)
- [ ] Step 5.2 — Commit with PREK_ALLOW_NO_CONFIG=1
- [ ] Step 5.3 — `git push -u agicash-rs spike/leptos-view-transitions`

### Task 6 — Cleanup worktree

- [ ] Step 6.1 — `ExitWorktree` with `action: keep` so the branch
  artifacts stay until the parent reviews
