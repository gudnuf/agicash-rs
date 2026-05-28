//! lucide-react icon mirrors.
//!
//! React renders its icons with `lucide-react`. Leptos has no icon crate
//! wired, so — exactly as `theme.rs` does for `Sun` / `Moon` / `SunMoon`
//! — we inline the SVG path data copied verbatim from lucide-react
//! v0.468.0 (the version pinned in `node_modules/lucide-react`). Each
//! icon is a 24-box, `stroke="currentColor"`, round-cap SVG so it inherits
//! the surrounding text color (the home header passes
//! `text-muted-foreground`).
//!
//! Icons here mirror the four React home-header glyphs
//! (`app/routes/_protected._index.tsx`): `GiftIcon`, `Scan`, `Clock`,
//! `UserCircle2` (an alias for lucide `CircleUserRound`).

use leptos::prelude::*;

/// Shared lucide SVG wrapper — 24-box, `currentColor` stroke, round caps.
/// `class` defaults to the lucide-react default render size when the
/// caller doesn't override (lucide's own default is 24px; the home header
/// uses the icon at its intrinsic size, matching React, which passes only
/// `className="text-muted-foreground"`).
macro_rules! lucide_svg {
    ($($child:tt)*) => {
        view! {
            <svg
                xmlns="http://www.w3.org/2000/svg"
                width="24"
                height="24"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
            >
                $($child)*
            </svg>
        }
    };
}

/// lucide `Gift`. Paths verbatim from lucide-react v0.468.0 `gift.js`.
#[component]
pub fn GiftIcon() -> impl IntoView {
    lucide_svg! {
        <rect x="3" y="8" width="18" height="4" rx="1"/>
        <path d="M12 8v13"/>
        <path d="M19 12v7a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-7"/>
        <path d="M7.5 8a2.5 2.5 0 0 1 0-5A4.8 8 0 0 1 12 8a4.8 8 0 0 1 4.5-5 2.5 2.5 0 0 1 0 5"/>
    }
}

/// lucide `Scan`. Paths verbatim from lucide-react v0.468.0 `scan.js`.
#[component]
pub fn ScanIcon() -> impl IntoView {
    lucide_svg! {
        <path d="M3 7V5a2 2 0 0 1 2-2h2"/>
        <path d="M17 3h2a2 2 0 0 1 2 2v2"/>
        <path d="M21 17v2a2 2 0 0 1-2 2h-2"/>
        <path d="M7 21H5a2 2 0 0 1-2-2v-2"/>
    }
}

/// lucide `Clock`. Paths verbatim from lucide-react v0.468.0 `clock.js`.
#[component]
pub fn ClockIcon() -> impl IntoView {
    lucide_svg! {
        <circle cx="12" cy="12" r="10"/>
        <polyline points="12 6 12 12 16 14"/>
    }
}

/// lucide `UserCircle2` (= `CircleUserRound`). Paths verbatim from
/// lucide-react v0.468.0 `circle-user-round.js`.
#[component]
pub fn UserCircleIcon() -> impl IntoView {
    lucide_svg! {
        <path d="M18 20a6 6 0 0 0-12 0"/>
        <circle cx="12" cy="10" r="4"/>
        <circle cx="12" cy="12" r="10"/>
    }
}
