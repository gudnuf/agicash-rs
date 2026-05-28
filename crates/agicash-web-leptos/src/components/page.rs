//! Page-shell primitives — `Page` / `PageHeader` / `PageContent`.
//!
//! Structural port of the React `app/components/page.tsx` shell so
//! future Leptos screens stop bleeding inline styles. Every primitive
//! consumes Tailwind tokens that resolve through the active body theme
//! class (`bg-background`, `text-foreground`, etc.), matching the live
//! Tailwind v4 pipeline.
//!
//! ## React → Leptos slot model
//!
//! React's `PageHeader` introspects its children for an `isHeaderItem`
//! marker and a `position` field (`left` / `center` / `right`) to bucket
//! them into a `grid-cols-[1fr_auto_1fr]` 3-column header. Leptos can't
//! reflect on child component types at runtime, so the same three slots
//! are surfaced as explicit optional `left` / `center` / `right` view
//! props. The rendered DOM is identical to React's:
//!
//! ```html
//! <header class="mb-4 grid h-7 w-full grid-cols-[1fr_auto_1fr] items-center">
//!   <div class="flex items-center justify-self-start"> {left} </div>
//!   <div class="flex items-center justify-self-center"> {center} </div>
//!   <div class="flex items-center gap-2 justify-self-end"> {right} </div>
//! </header>
//! ```
//!
//! `PageHeaderItem` is kept as a thin pass-through wrapper (matching
//! React's `<div className={className}>{children}</div>`) so callers can
//! group several links into one slot with shared spacing classes — e.g.
//! the home header's `flex gap-6` clusters.

use leptos::prelude::*;

/// Outer page frame. Mirrors React `Page`:
/// `mx-auto flex h-dvh w-full flex-col p-4 font-primary
///  sm:items-center sm:px-6 lg:px-8`.
///
/// `font-primary` resolves to the Kode Mono brand face via the Tailwind
/// theme tokens; the existing shell uses `font-mono` for the same face,
/// so we mirror React's class verbatim here and let the bundle resolve
/// it (both alias the `--font-primary` token).
#[component]
pub fn Page(
    /// Extra classes appended to the base frame classes.
    #[prop(into, optional)]
    class: Option<String>,
    children: Children,
) -> impl IntoView {
    let base = "mx-auto flex h-dvh w-full flex-col p-4 font-mono \
                sm:items-center sm:px-6 lg:px-8";
    let class = merge(base, class);
    view! { <div class=class>{children()}</div> }
}

/// One header slot's content wrapper. Pass-through `<div>` so callers can
/// cluster links with shared spacing (e.g. `class="flex gap-6"`), exactly
/// like React's `PageHeaderItem`.
#[component]
pub fn PageHeaderItem(
    /// Classes applied to the wrapping `<div>`.
    #[prop(into, optional)]
    class: Option<String>,
    children: Children,
) -> impl IntoView {
    view! { <div class=class>{children()}</div> }
}

/// 3-column header. Mirrors React `PageHeader`:
/// `mb-4 grid h-7 w-full grid-cols-[1fr_auto_1fr] items-center`, with
/// left / center / right buckets. Each slot is optional; an absent slot
/// still renders its (empty) grid cell so the center column stays
/// centered.
#[component]
pub fn PageHeader(
    /// Extra classes appended to the base header classes.
    #[prop(into, optional)]
    class: Option<String>,
    /// Left-aligned slot (`justify-self-start`).
    #[prop(optional, into)]
    left: Option<ViewFn>,
    /// Center slot (`justify-self-center`).
    #[prop(optional, into)]
    center: Option<ViewFn>,
    /// Right-aligned slot (`justify-self-end`, `gap-2`).
    #[prop(optional, into)]
    right: Option<ViewFn>,
) -> impl IntoView {
    let base = "mb-4 grid h-7 w-full grid-cols-[1fr_auto_1fr] items-center";
    let class = merge(base, class);
    view! {
        <header class=class>
            <div class="flex items-center justify-self-start">
                {left.map(|v| v.run())}
            </div>
            <div class="flex items-center justify-self-center">
                {center.map(|v| v.run())}
            </div>
            <div class="flex items-center gap-2 justify-self-end">
                {right.map(|v| v.run())}
            </div>
        </header>
    }
}

/// Main content region. Mirrors React `PageContent`:
/// `flex flex-grow flex-col gap-2 p-2 sm:w-full sm:max-w-sm`.
#[component]
pub fn PageContent(
    /// Extra classes appended to the base content classes.
    #[prop(into, optional)]
    class: Option<String>,
    children: Children,
) -> impl IntoView {
    let base = "flex flex-grow flex-col gap-2 p-2 sm:w-full sm:max-w-sm";
    let class = merge(base, class);
    view! { <main class=class>{children()}</main> }
}

/// Append caller classes to a component's base classes (React's `cn`
/// behaviour for our static-base + optional-override case). No conflict
/// resolution — Tailwind's later-class-wins ordering handles overrides,
/// same as `cn` without `tailwind-merge` would.
fn merge(base: &str, extra: Option<String>) -> String {
    match extra {
        Some(extra) if !extra.is_empty() => format!("{base} {extra}"),
        _ => base.to_string(),
    }
}
