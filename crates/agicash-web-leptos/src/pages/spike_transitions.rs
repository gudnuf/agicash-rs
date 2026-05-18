//! Spike demo routes for the View Transitions integration.
//!
//! Mounted at `/spike/transitions/{a,b,sheet}`. Each page is a
//! deliberately-minimal placeholder whose only job is to exercise
//! the [`crate::transitions::AnimatedA`] component:
//!
//!   - **A** ↔ **B**: forward (`SlideLeft`) and back (`SlideRight`) +
//!     shared-element morph of the colored "hero" circle.
//!   - **A** → **A** (self-link): `Fade` cross.
//!   - **A** → **Sheet**: `SlideUp` (modal-style sheet enter).
//!   - **Sheet** → **A**: `SlideDown` (sheet dismiss).
//!
//! These pages do NOT depend on auth or wallet data — they render
//! flat under the protected layout so the existing app shell + bottom
//! nav stay visible. (You'll need to log in to reach them; the
//! protected layout redirects to /login when no access token is
//! present.)

use leptos::prelude::*;

use crate::tokens;
use crate::transitions::{AnimatedA, Transition};

/// Top of page A. The "hero" circle here carries
/// `view-transition-name: spike-hero` so the browser can morph it
/// into the matching circle on page B.
#[component]
pub fn SpikeAPage() -> impl IntoView {
    view! {
        <div style=page_style("hsl(220 70% 96%)")>
            <h1 style=title_style()>"Page A"</h1>
            <p style=meta_style()>
                "Click a link below to see the matching transition. The "
                "red circle morphs into Page B's bigger circle via a "
                "named view-transition."
            </p>

            // The shared-element hero. Inline `view-transition-name`
            // is what couples this DOM node to the matching node on
            // page B.
            <div style=hero_circle_style(48)/>

            <div style=button_row_style()>
                <AnimatedA href="/spike/transitions/b" transition=Transition::SlideLeft>
                    <span style=link_button_style("hsl(220 70% 50%)")>"→ Page B (slideLeft)"</span>
                </AnimatedA>
                <AnimatedA href="/spike/transitions/sheet" transition=Transition::SlideUp>
                    <span style=link_button_style("hsl(160 50% 45%)")>"⤴ Sheet (slideUp)"</span>
                </AnimatedA>
                <AnimatedA href="/spike/transitions/a" transition=Transition::Fade>
                    <span style=link_button_style("hsl(0 0% 35%)")>"⟳ Self (fade)"</span>
                </AnimatedA>
            </div>
        </div>
    }
}

/// Page B — receiving end of the SlideLeft transition from A. The
/// hero circle is bigger and re-positioned; the browser interpolates
/// position + size automatically because both circles share
/// `view-transition-name: spike-hero`.
#[component]
pub fn SpikeBPage() -> impl IntoView {
    view! {
        <div style=page_style("hsl(0 70% 96%)")>
            <h1 style=title_style()>"Page B"</h1>
            <p style=meta_style()>
                "The circle morphed from Page A's small hero. Back to A "
                "uses slideRight — the reverse of how we arrived."
            </p>

            // Same `view-transition-name`, different size + position
            // → the browser interpolates the geometry.
            <div style=hero_circle_style(160)/>

            <div style=button_row_style()>
                <AnimatedA href="/spike/transitions/a" transition=Transition::SlideRight>
                    <span style=link_button_style("hsl(220 70% 50%)")>"← Page A (slideRight)"</span>
                </AnimatedA>
            </div>
        </div>
    }
}

/// Sheet — a modal-style page that animates in from the bottom
/// (`SlideUp`) and dismisses back down (`SlideDown`). Visually
/// suggests a vaul-style drawer without actually being one.
#[component]
pub fn SpikeSheetPage() -> impl IntoView {
    view! {
        <div style=sheet_style()>
            <h1 style=title_style()>"Modal Sheet"</h1>
            <p style=meta_style()>
                "Slid up from the bottom edge. Closing slides it back "
                "down. This mirrors the React app's /receive, /send, "
                "/buy modal pattern."
            </p>
            <div style=button_row_style()>
                <AnimatedA href="/spike/transitions/a" transition=Transition::SlideDown>
                    <span style=link_button_style("hsl(0 0% 35%)")>"× Close (slideDown)"</span>
                </AnimatedA>
            </div>
        </div>
    }
}

// ---- Styles ---------------------------------------------------------------

fn page_style(bg: &str) -> String {
    format!(
        "display:flex; flex-direction:column; align-items:center; \
         gap:{}; padding:{}; min-height:60dvh; background:{}; \
         font-family:{};",
        tokens::SPACE_L,
        tokens::SPACE_XXL,
        bg,
        tokens::FONT_PRIMARY,
    )
}

fn sheet_style() -> String {
    format!(
        "display:flex; flex-direction:column; align-items:center; \
         gap:{}; padding:{}; min-height:60dvh; background:{}; \
         font-family:{}; border-top-left-radius:24px; \
         border-top-right-radius:24px; box-shadow:0 -12px 32px rgba(0,0,0,0.15);",
        tokens::SPACE_L,
        tokens::SPACE_XXL,
        "hsl(45 80% 95%)",
        tokens::FONT_PRIMARY,
    )
}

fn title_style() -> String {
    format!(
        "margin:0; font-size:32px; font-weight:600; color:{};",
        tokens::COLOR_FOREGROUND
    )
}

fn meta_style() -> String {
    format!(
        "max-width:32ch; margin:0; text-align:center; font-size:13px; \
         line-height:1.5; color:{};",
        tokens::COLOR_MUTED_FOREGROUND
    )
}

fn button_row_style() -> String {
    format!(
        "display:flex; flex-direction:column; gap:{}; width:100%; max-width:24rem;",
        tokens::SPACE_M
    )
}

fn link_button_style(color: &str) -> String {
    format!(
        "display:block; padding:{} {}; border-radius:12px; \
         background:{}; color:#fff; text-align:center; \
         font-weight:600; font-size:15px;",
        tokens::SPACE_M,
        tokens::SPACE_L,
        color,
    )
}

/// `diameter` in CSS pixels. The shared-element transition uses
/// `view-transition-name: spike-hero` so the browser auto-morphs
/// position + size between page A's small circle and page B's larger
/// one.
fn hero_circle_style(diameter: u32) -> String {
    format!(
        "width:{diameter}px; height:{diameter}px; border-radius:50%; \
         background:radial-gradient(circle at 30% 30%, hsl(0 90% 70%), hsl(0 80% 45%)); \
         box-shadow:0 8px 16px rgba(0,0,0,0.15); \
         view-transition-name:spike-hero;"
    )
}
