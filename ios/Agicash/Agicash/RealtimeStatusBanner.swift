import SwiftUI

/// Thin top-of-screen banner that reflects the Rust realtime supervisor's
/// `RealtimeStatusFfi` (forwarded from `WalletEventBridge.onStatus`). The
/// banner is positioned above the main signed-in content (see
/// `ContentView.swift`'s `AuthGateView`) and is purely additive — it
/// never blocks layout: it occupies zero height when the channel is
/// healthy (`.subscribed` / `.connecting` after first subscribe) and
/// only takes a thin strip otherwise.
///
/// State map (mirrors the Lane 1 audit document
/// `2026-05-19-realtime-parity.md`):
///
///   - `.idle` / `.subscribed` / `.closed`              → hidden (no banner)
///   - `.connecting` / `.reconnecting` / `.error`       → "Reconnecting…"
///     muted strip (transient; the supervisor will resolve on its own)
///   - `.terminalError`                                 → persistent
///     destructive strip with "Tap to retry" affordance that calls
///     `WalletViewModel.retryRealtime()` — the supervisor's online-edge
///     handler clears the latched `terminal` flag on the false→true
///     transition and resumes the reconnect ladder.
///
/// Typography: brand `Font.brandCaption` (Kode Mono 12pt) so it sits
/// quietly under the system status bar without competing with the
/// `BalanceHero`. Colors come straight from the Asset Catalog brand
/// tokens (`.brandMuted`/`.brandMutedForeground` for transient,
/// `.brandDestructive`/`.brandDestructiveForeground` for terminal) so
/// the banner is dark-mode-aware without bespoke logic.
struct RealtimeStatusBanner: View {
    @Bindable var model: WalletViewModel

    var body: some View {
        switch bannerKind(for: model.realtimeStatus) {
        case .hidden:
            // `EmptyView` keeps the parent layout pristine — no spacer,
            // no padding, no layout pass cost beyond a SwiftUI view-tree
            // node. The signed-in shell measures identically to today
            // when the channel is healthy.
            EmptyView()
        case .transient:
            TransientBanner()
                // Subtle slide-in so the banner doesn't pop layout on
                // first appearance. Matches the cadence of the existing
                // SwiftUI sheet/`.refreshable` transitions in the app.
                .transition(.move(edge: .top).combined(with: .opacity))
        case .terminal:
            TerminalBanner(model: model)
                .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    /// Three-way collapse of `RealtimeStatusFfi` into the visual states
    /// the banner actually renders. Centralising this keeps the switch
    /// exhaustive (a new FFI variant produces a compiler error here)
    /// and decouples copy/style from variant identity.
    private enum Kind { case hidden, transient, terminal }
    private func bannerKind(for status: RealtimeStatusFfi) -> Kind {
        switch status {
        case .idle, .subscribed, .closed:
            return .hidden
        case .connecting, .reconnecting, .error:
            return .transient
        case .terminalError:
            return .terminal
        }
    }
}

/// Muted "Reconnecting…" strip rendered while the Rust supervisor is
/// mid-reconnect ladder. No tap target — the supervisor is already
/// trying.
private struct TransientBanner: View {
    var body: some View {
        HStack(spacing: Spacing.s) {
            ProgressView()
                // Match the muted-foreground tone instead of the system
                // accent so the spinner reads as informational, not as a
                // primary call-to-action.
                .controlSize(.mini)
                .tint(Color.brandMutedForeground)
            Text("Reconnecting…")
                .font(.brandCaption)
                .foregroundStyle(Color.brandMutedForeground)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Spacing.s)
        .padding(.horizontal, Spacing.l)
        .background(Color.brandMuted)
        // Thin hairline below so the banner reads as a distinct strip
        // rather than blending into the home background.
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.brandBorder)
                .frame(height: 0.5)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Reconnecting to realtime")
    }
}

/// Persistent destructive strip rendered when the supervisor latched
/// `TerminalError` (9 consecutive `JoinReplyError` attempts — the F13
/// dampening lever, see `MAX_JOIN_REJECT_ATTEMPTS` in
/// `agicash-realtime/src/service.rs`). The whole strip is a button so a
/// thumb anywhere on the bar triggers retry.
private struct TerminalBanner: View {
    @Bindable var model: WalletViewModel

    var body: some View {
        Button {
            Task { await model.retryRealtime() }
        } label: {
            HStack(spacing: Spacing.s) {
                if model.realtimeRetryInFlight {
                    // Inline spinner so a tap registers visually even if
                    // the supervisor takes a beat to flip status back to
                    // `connecting`/`subscribed`. Tinted with the
                    // destructive-foreground so it stays legible on the
                    // red strip.
                    ProgressView()
                        .controlSize(.mini)
                        .tint(Color.brandDestructiveForeground)
                    Text("Reconnecting…")
                        .font(.brandCaption)
                        .foregroundStyle(Color.brandDestructiveForeground)
                } else {
                    Image(systemName: "wifi.exclamationmark")
                        .font(.caption)
                        .foregroundStyle(Color.brandDestructiveForeground)
                    Text("Connection lost — tap to retry")
                        .font(.brandCaption)
                        .foregroundStyle(Color.brandDestructiveForeground)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Spacing.s)
            .padding(.horizontal, Spacing.l)
            .background(Color.brandDestructive)
            .contentShape(Rectangle())
        }
        // `.plain` so the banner doesn't acquire the default iOS bordered
        // button visuals — we own the look ourselves.
        .buttonStyle(.plain)
        .accessibilityHint("Tap to retry the realtime connection")
    }
}
