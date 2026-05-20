import Network
import SwiftUI

@main
struct AgicashApp: App {
    @State private var walletState: WalletState
    /// Tracks reachability via `NWPathMonitor` and forwards online/offline
    /// transitions into the wallet's realtime supervisor (audit
    /// `2026-05-19-realtime-parity.md` Gap-E). Started once at app launch
    /// and runs for the process lifetime — symmetric with the realtime
    /// subscription itself, which lives from session start to sign-out.
    @State private var reachability = ReachabilityMonitor()
    /// SwiftUI's app-wide scene phase. `.active`/`.inactive`/`.background`
    /// transitions are forwarded to the realtime supervisor as
    /// `setRealtimeActive(true/false)` — backgrounded socket closes
    /// (battery-friendly), foregrounding resumes + fires `onConnected`'s
    /// catch-up refetch.
    @Environment(\.scenePhase) private var scenePhase

    init() {
        self.walletState = WalletState()
    }

    var body: some Scene {
        WindowGroup {
            ContentView(state: walletState)
                .task { await walletState.bootstrap() }
                .task {
                    // Drive the reachability stream for the wallet's
                    // lifetime. `for await` keeps the task alive until
                    // the view goes away (app termination); each
                    // boolean is forwarded once on every transition.
                    for await online in reachability.stream() {
                        if case .ready(let model) = walletState.result {
                            await model.setRealtimeOnline(online)
                        }
                    }
                }
                .onChange(of: scenePhase, initial: false) { _, newValue in
                    let isActive = (newValue == .active)
                    Task { @MainActor in
                        if case .ready(let model) = walletState.result {
                            await model.setRealtimeActive(isActive)
                        }
                    }
                }
        }
    }
}

/// Wraps `NWPathMonitor` as an `AsyncStream<Bool>` of reachability
/// transitions. Emits `true` whenever the OS reports a usable path
/// and `false` whenever it reports `.unsatisfied`. Deduplicates by
/// definition (only fires when `path.status` differs from the last
/// emitted value). Mirrors what React's
/// `useSupabaseRealtimeActivityTracking` gets for free from
/// `navigator.onLine` + `window` `online`/`offline` events.
///
/// Lifetime: created once in `AgicashApp`, started by the `.task`
/// modifier on `ContentView`; the monitor lives until the process
/// exits. Cancellation of the stream consumer cancels the monitor.
@MainActor
final class ReachabilityMonitor {
    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "app.agicash.reachability")

    /// Yields a new boolean on every reachability transition. The
    /// initial value is also yielded so the supervisor learns the
    /// state at startup (typically `true` on a successful launch).
    func stream() -> AsyncStream<Bool> {
        AsyncStream { continuation in
            // Capture last value so we don't yield on no-op updates.
            var last: Bool? = nil
            monitor.pathUpdateHandler = { path in
                let online = (path.status == .satisfied)
                if last != online {
                    last = online
                    continuation.yield(online)
                }
            }
            continuation.onTermination = { [monitor] _ in
                monitor.cancel()
            }
            monitor.start(queue: queue)
        }
    }
}

/// Thin wrapper that defers `WalletViewModel` initialization (which can fail
/// if the underlying FFI rejects the configured URLs) and lets ContentView
/// pull either a model or a fatal error out of it.
@MainActor
@Observable
final class WalletState {
    enum BootResult {
        case pending
        case ready(WalletViewModel)
        case failed(String)
    }

    var result: BootResult = .pending

    func bootstrap() async {
        switch result {
        case .pending:
            break
        case .ready, .failed:
            return
        }
        do {
            let model = try WalletViewModel()
            result = .ready(model)
            if !applyDemoSignedInIfRequested(model: model) {
                await model.bootstrap()
            }
        } catch {
            result = .failed("init failed: \(error)")
        }
    }

    /// Debug-only: when launched with `-AgicashDemoSignedIn YES` (CLI flag
    /// or Xcode scheme argument), skip the bootstrap call to OpenSecret +
    /// Supabase and seed a fake signed-in state with mock accounts. Used to
    /// capture screenshots of the home / settings flow without standing up
    /// the local enclave + Supabase stack.
    ///
    /// Production behaviour is unchanged: the flag is read from the process
    /// argument list, which the Phase 1 dev simulator launch can pass but a
    /// real device install never receives.
    @discardableResult
    private func applyDemoSignedInIfRequested(model: WalletViewModel) -> Bool {
        #if DEBUG
        let argsHasDemo = CommandLine.arguments.contains("-AgicashDemoSignedIn")
            || ProcessInfo.processInfo.arguments.contains("-AgicashDemoSignedIn")
            || (UserDefaults.standard.string(forKey: "AgicashDemoSignedIn") == "YES")
        guard argsHasDemo else { return false }
        model.isDemoMode = true
        model.phase = .signedIn(userId: "11111111-2222-3333-4444-555555555555")
        model.accounts = [
            AccountFfi(
                id: "demo-cashu-btc",
                name: "Default BTC",
                accountType: "cashu",
                currency: "BTC",
                mintUrl: "https://mint.agicash.dev",
                balance: "0",
                unit: "sat"
            ),
            AccountFfi(
                id: "demo-cashu-usd",
                name: "Default USD",
                accountType: "cashu",
                currency: "USD",
                mintUrl: "https://mint.agicash.dev",
                balance: "0",
                unit: "cent"
            ),
            AccountFfi(
                id: "demo-spark-btc",
                name: "Spark Lightning",
                accountType: "spark",
                currency: "BTC",
                mintUrl: nil,
                balance: "0",
                unit: "sat"
            ),
        ]
        return true
        #else
        return false
        #endif
    }
}
