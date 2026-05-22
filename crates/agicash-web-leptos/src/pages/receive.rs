//! `/receive` — receive hub, three-tab surface.
//!
//! Mirrors the iOS `ReceiveCarouselView` and Android
//! `ReceiveCarouselScreen` shape: a three-tab hub
//! (Cashu / Lightning / Dollar). Web differs from mobile in that there's
//! no swipeable pager — tabs are clicked, not swiped — but the tab
//! order, semantics, and "Coming soon" affordances match.
//!
//! Today only the Cashu tab is wired: it renders the existing
//! [`CashuTokenPasteView`] (lane L4). Lightning and Dollar tabs are
//! visible but disabled with "Coming soon" placeholders; they will be
//! filled by slice 12 (mint-quote / lightning-receive) and a future
//! fiat-onramp lane respectively.
//!
//! Routing: this page is reached at `/receive` (main bottom nav). The
//! standalone sub-route `/receive/cashu` (`ReceiveCashuPage`) is kept
//! for backwards compatibility with deep-links and the React-app URL
//! shape, but the bottom-nav entry now lands here instead of on a
//! placeholder.

use leptos::prelude::*;

use crate::components::CashuTokenPasteView;
use crate::tokens;

/// Tabs the receive hub surfaces. Order + labels mirror iOS
/// (`ReceiveTab`: cashu, lightning, buy) and Android
/// (`ReceiveTab`: CASHU, LIGHTNING, BUY). The web label "Dollar" is
/// the short form of "Buy sats" — the operator-requested wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveTab {
    Cashu,
    Lightning,
    Dollar,
}

impl ReceiveTab {
    fn label(self) -> &'static str {
        match self {
            ReceiveTab::Cashu => "Cashu",
            ReceiveTab::Lightning => "Lightning",
            ReceiveTab::Dollar => "Dollar",
        }
    }

    /// Is this tab wired to a real flow, or just a "Coming soon"
    /// placeholder? Today only Cashu is real.
    fn enabled(self) -> bool {
        matches!(self, ReceiveTab::Cashu)
    }
}

#[component]
pub fn ReceivePage() -> impl IntoView {
    // Selected tab. Defaults to Cashu — the only wired tab today, and
    // the operator's primary use case. iOS defaults to Lightning; the
    // web's default differs because Lightning isn't wired yet, so
    // landing on a "Coming soon" placeholder would be a worse first
    // impression than landing on a working paste-token view.
    let selected = RwSignal::new(ReceiveTab::Cashu);

    view! {
        <div style=page_style()>
            <TabBar selected=selected/>
            <TabPanel selected=selected/>
        </div>
    }
}

/// Horizontal tab strip. Clicking an enabled tab updates `selected`;
/// disabled tabs (Lightning, Dollar) still respond to clicks so the
/// user can see the "Coming soon" body, but render in a muted style.
#[component]
fn TabBar(selected: RwSignal<ReceiveTab>) -> impl IntoView {
    let tabs = [ReceiveTab::Cashu, ReceiveTab::Lightning, ReceiveTab::Dollar];

    view! {
        <div style=tab_bar_style() role="tablist">
            {tabs
                .into_iter()
                .map(|tab| view! { <TabButton tab=tab selected=selected/> })
                .collect_view()}
        </div>
    }
}

#[component]
fn TabButton(tab: ReceiveTab, selected: RwSignal<ReceiveTab>) -> impl IntoView {
    // Reactive style: derive both "is selected" and "is enabled" off
    // signals so a click on a disabled tab flips the visual selection
    // without re-rendering the tab strip.
    let is_selected = Memo::new(move |_| selected.get() == tab);
    let style = move || tab_button_style(is_selected.get(), tab.enabled());
    let label = tab.label();
    let enabled = tab.enabled();

    view! {
        <button
            type="button"
            role="tab"
            aria-selected=move || is_selected.get().to_string()
            style=style
            on:click=move |_| selected.set(tab)
        >
            <span>{label}</span>
            {(!enabled)
                .then(|| view! { <span style=coming_soon_badge_style()>"Coming soon"</span> })}
        </button>
    }
}

/// Tab body. Switches on the selected tab; Cashu renders the wired
/// `CashuTokenPasteView`, the other two render a "Coming soon" card.
#[component]
fn TabPanel(selected: RwSignal<ReceiveTab>) -> impl IntoView {
    view! {
        <div role="tabpanel" style=tab_panel_style()>
            {move || match selected.get() {
                ReceiveTab::Cashu => view! { <CashuTokenPasteView/> }.into_any(),
                ReceiveTab::Lightning => view! {
                    <ComingSoonCard
                        title="Receive Lightning"
                        body="Pay-to-invoice receives over Lightning are coming soon. \
                              For now, paste a Cashu token to claim it."
                    />
                }
                .into_any(),
                ReceiveTab::Dollar => view! {
                    <ComingSoonCard
                        title="Buy sats"
                        body="Fiat onramp is coming soon. For now, paste a Cashu token \
                              to claim it."
                    />
                }
                .into_any(),
            }}
        </div>
    }
}

/// Shared placeholder card for the not-yet-wired tabs. Mirrors the
/// shape of the Android `BuyPlaceholderPage` (titled card on a centered
/// column with an explanatory line + a disabled CTA).
#[component]
fn ComingSoonCard(title: &'static str, body: &'static str) -> impl IntoView {
    view! {
        <div style=coming_soon_wrap_style()>
            <div style=coming_soon_card_style()>
                <h2 style=coming_soon_title_style()>{title}</h2>
                <p style=coming_soon_body_style()>{body}</p>
                <button type="button" style=coming_soon_button_style() disabled=true>
                    "Coming soon"
                </button>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------
//
// Inline CSS strings, same convention as the rest of this crate (see
// `tokens.rs` rationale). Values pulled from `tokens.rs`; structure
// inspired by the mobile carousels' bottom indicator bar but adapted
// to a top tab strip (web doesn't have a swipe gesture to indicate).

fn page_style() -> String {
    format!(
        "display:flex; flex-direction:column; min-height:100%; \
         background:{};",
        tokens::COLOR_BACKGROUND,
    )
}

fn tab_bar_style() -> String {
    format!(
        "display:flex; flex-direction:row; align-items:stretch; \
         border-bottom:1px solid {}; background:{};",
        tokens::COLOR_BORDER,
        tokens::COLOR_BACKGROUND,
    )
}

fn tab_button_style(is_selected: bool, is_enabled: bool) -> String {
    let (color, weight, border_color) = if is_selected {
        (tokens::COLOR_FOREGROUND, "600", tokens::COLOR_FOREGROUND)
    } else if is_enabled {
        (tokens::COLOR_MUTED_FOREGROUND, "500", "transparent")
    } else {
        // Disabled, unselected: same muted tone as the iOS indicator
        // bar's `mutedForeground.opacity(0.4)`.
        (tokens::COLOR_MUTED_FOREGROUND, "500", "transparent")
    };
    let opacity = if is_enabled || is_selected {
        "1"
    } else {
        "0.6"
    };
    format!(
        "flex:1 1 0; display:inline-flex; flex-direction:column; \
         align-items:center; justify-content:center; gap:{gap}; \
         padding:{vpad} {hpad}; border:none; background:transparent; \
         border-bottom:2px solid {border_color}; color:{color}; \
         font-family:{font}; font-size:{size}; font-weight:{weight}; \
         cursor:pointer; opacity:{opacity};",
        gap = tokens::SPACE_XS,
        vpad = tokens::SPACE_M,
        hpad = tokens::SPACE_L,
        font = tokens::FONT_PRIMARY,
        size = tokens::TEXT_BASE,
        weight = weight,
        color = color,
        border_color = border_color,
        opacity = opacity,
    )
}

fn coming_soon_badge_style() -> String {
    format!(
        "font-size:0.625rem; font-weight:500; \
         color:{}; background:{}; \
         padding:2px {}; border-radius:{}; \
         text-transform:uppercase; letter-spacing:0.05em;",
        tokens::COLOR_MUTED_FOREGROUND,
        tokens::COLOR_MUTED,
        tokens::SPACE_S,
        tokens::RADIUS_MD,
    )
}

fn tab_panel_style() -> String {
    // Flex-grow so the panel fills the remaining height under the
    // tab strip — the wired `CashuTokenPasteView` already centers
    // its card inside whatever box it gets.
    "flex:1 1 auto; display:flex; flex-direction:column;".to_string()
}

fn coming_soon_wrap_style() -> String {
    format!(
        "display:flex; flex:1 1 auto; align-items:center; \
         justify-content:center; padding:{};",
        tokens::SPACE_XL,
    )
}

fn coming_soon_card_style() -> String {
    format!(
        "display:flex; flex-direction:column; gap:{gap}; \
         width:100%; max-width:{max_w}; padding:{pad}; \
         background:{bg}; color:{fg}; border:1px solid {border}; \
         border-radius:{radius}; box-shadow:{shadow}; \
         align-items:flex-start;",
        gap = tokens::SPACE_L,
        max_w = tokens::CARD_MAX_WIDTH,
        pad = tokens::SPACE_XXL,
        bg = tokens::COLOR_CARD,
        fg = tokens::COLOR_CARD_FOREGROUND,
        border = tokens::COLOR_BORDER,
        radius = tokens::RADIUS_LG,
        shadow = tokens::SHADOW_XS,
    )
}

fn coming_soon_title_style() -> String {
    format!(
        "margin:0; font-size:{}; font-weight:600; color:{}; \
         font-family:{};",
        tokens::TEXT_LG,
        tokens::COLOR_CARD_FOREGROUND,
        tokens::FONT_PRIMARY,
    )
}

fn coming_soon_body_style() -> String {
    format!(
        "margin:0; font-size:{}; color:{}; font-family:{}; \
         line-height:1.5;",
        tokens::TEXT_SM,
        tokens::COLOR_MUTED_FOREGROUND,
        tokens::FONT_PRIMARY,
    )
}

fn coming_soon_button_style() -> String {
    format!(
        "display:inline-flex; align-items:center; justify-content:center; \
         padding:{vpad} {hpad}; border:1px solid {border}; \
         border-radius:{radius}; background:{bg}; color:{fg}; \
         font-family:{font}; font-size:{size}; font-weight:500; \
         cursor:not-allowed; opacity:0.6; align-self:stretch;",
        vpad = tokens::SPACE_M,
        hpad = tokens::SPACE_L,
        border = tokens::COLOR_BORDER,
        radius = tokens::RADIUS_MD,
        bg = tokens::COLOR_MUTED,
        fg = tokens::COLOR_MUTED_FOREGROUND,
        font = tokens::FONT_PRIMARY,
        size = tokens::TEXT_SM,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cashu_tab_is_wired() {
        assert!(ReceiveTab::Cashu.enabled());
    }

    #[test]
    fn coming_soon_tabs_are_disabled() {
        assert!(!ReceiveTab::Lightning.enabled());
        assert!(!ReceiveTab::Dollar.enabled());
    }

    #[test]
    fn tab_labels_match_operator_spec() {
        assert_eq!(ReceiveTab::Cashu.label(), "Cashu");
        assert_eq!(ReceiveTab::Lightning.label(), "Lightning");
        assert_eq!(ReceiveTab::Dollar.label(), "Dollar");
    }
}
