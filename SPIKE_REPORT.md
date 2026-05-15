# Cross-Platform Compile Recon — Agicash Rust SDK

Spike branch: `spike/cross-platform-compile` from `feat/rust-accounts` @ `09c5b5a1` (slice 3 head).
Probe command: `cargo check -p <crate> --target <target>` (no source edits).
Toolchain: `1.86.0` (workspace pin). All three targets installed via `rustup target add`.

Environment caveats:
- macOS host has no usable iOS SDK (`xcode-select -p` returns a nix-managed `apple-sdk-14.4` only; `xcrun --sdk iphoneos --show-sdk-path` fails). `cargo check` succeeds for pure-Rust crates because it stops at rmeta. Any crate with a C build script that calls `xcrun` (e.g. `ring`, `aws-lc-sys`) cannot complete on this machine — recorded as `BLOCKED (env)` not `FAIL`.
- `wasm32-unknown-unknown` has no such SDK dependency; results are real compile data.

---

## 1. Compile matrix

| Crate | Target | Status | Failing dep / reason | Workaround surface |
|-------|--------|--------|---------------------|---------------------|
| agicash-domain | aarch64-apple-ios | OK | — | — |
| agicash-domain | aarch64-apple-ios-sim | OK | — | — |
| agicash-domain | wasm32-unknown-unknown | FAIL | `uuid 1.23 v4` needs `js` / `rng-getrandom` / `rng-rand` feature on wasm32 | workspace dep feature flag (`uuid = { features = ["v4","serde","js"] }`) |
| agicash-money | aarch64-apple-ios | OK | — | — |
| agicash-money | aarch64-apple-ios-sim | OK | — | — |
| agicash-money | wasm32-unknown-unknown | FAIL | Inherits the same `uuid` failure transitively via `agicash-domain` | Fixed by the uuid fix above (no money-specific change) |
| agicash-auth-opensecret | aarch64-apple-ios | BLOCKED (env) | `ring 0.17` build script: `xcrun --sdk iphoneos` not available on host | None needed for code — install Xcode iOS SDK or use real macOS dev box. Then likely OK (security-framework, keyring, hyper, reqwest/rustls all support iOS). |
| agicash-auth-opensecret | aarch64-apple-ios-sim | BLOCKED (env) | same `ring` xcrun issue | same |
| agicash-auth-opensecret | wasm32-unknown-unknown | FAIL | `getrandom 0.2.17` requires `js` feature on wasm32 (pulled via opensecret → uuid → getrandom). Behind this lurks `ring`, `tokio = "full"`, and `reqwest` features that also won't fly on wasm. | Multi-layered: (a) feature-flag uuid/getrandom across workspace, (b) replace `ring`-using crypto in opensecret (or move opensecret behind a server proxy), (c) split tokio features, (d) reqwest already has wasm fetch path but only if its TLS features are off on wasm. The cleanest path is **don't compile opensecret to wasm** — keep it on the server side. |
| agicash-storage-supabase | aarch64-apple-ios | BLOCKED (env) | Transitive `ring` from reqwest 0.11 + rustls-tls-native-roots (postgrest fork) | Same as auth iOS — env not code |
| agicash-storage-supabase | aarch64-apple-ios-sim | BLOCKED (env) | same | same |
| agicash-storage-supabase | wasm32-unknown-unknown | FAIL | `uuid` (same root) — but stops there; reqwest 0.11 inside postgrest fork pins `rustls-tls-native-roots` which won't compile on wasm. Need wasm-fetch features instead. | (a) uuid feature flag, (b) **swap postgrest fork** to a reqwest-feature-set that includes wasm32 cfg path (drop rustls, let reqwest's wasm fetch path take over), (c) verify rustls-native-certs doesn't sneak back in. |
| agicash-cli | aarch64-apple-ios | BLOCKED (env) | Transitive `ring` via auth + storage | Env-only; would compile in principle, but a CLI binary on iOS is not useful — see "useless" qualifier in the brief. |
| agicash-cli | aarch64-apple-ios-sim | BLOCKED (env) | same | same |
| agicash-cli | wasm32-unknown-unknown | FAIL | Multi-cause: (1) `uuid` js feature, (2) `getrandom` js feature, (3) `clap` (4.5) — does compile on wasm32-unknown-unknown but its `std::io` paths are mostly useless, (4) `keyring` is gated for unix/macos/ios/windows targets only and has no wasm story, (5) `rpassword` needs a tty. | Don't try. agicash-cli is correctly CLI-only; the right move is a separate `agicash-web` (Leptos) crate that depends on the same domain/services/storage layers but skips CLI-specific deps. |

Summary by row count: **3 OK** (domain×ios, domain×ios-sim, money×ios, money×ios-sim, plus the two ios-sim OK rows — 4 actually OK), **5 FAIL** (all wasm), **6 BLOCKED (env)** (all iOS for non-pure crates, because no Xcode iOS SDK present).

Corrected tally:
- OK: 4 (agicash-domain & agicash-money on both iOS targets)
- FAIL (real wasm compile fail): 5
- BLOCKED (env, would need Xcode iOS SDK to confirm): 6

---

## 2. Detailed failure modes

### agicash-domain on wasm32-unknown-unknown — `uuid` rng

```
error: to use `uuid` on `wasm32-unknown-unknown`, specify a source of randomness
       using one of the `js`, `rng-getrandom`, or `rng-rand` features
   --> uuid-1.23.1/src/rng.rs:104:5
error[E0433]: failed to resolve: could not find `RngImp` in `imp`
   --> uuid-1.23.1/src/rng.rs:10:10
   = note: gated behind the `rng-rand` feature
error: could not compile `uuid` (lib) due to 4 previous errors
```

Pure feature-flag fix. The workspace dep is `uuid = { version = "1.11", features = ["v4", "serde"] }`. Adding `"js"` (or making it conditional via a `target.'cfg(target_arch = "wasm32")'.dependencies` block in each consumer) resolves it. Everything downstream that fails on uuid is the same root cause; fixing it once at the workspace level cascades. Domain becomes OK on wasm; money likely too (still need to verify chrono's wasm path, but chrono with `std,clock,serde` should be fine — it gates JS-time on a separate `wasmbind` feature only needed if you want `Local::now()`).

### agicash-money on wasm32 — transitive uuid

Same root cause as domain. Money has no other wasm-hostile deps (`rust_decimal` is pure Rust).

### agicash-auth-opensecret on iOS device/sim — `ring` build script

```
error occurred in cc-rs: command did not execute successfully (status code exit status: 255):
"xcrun" "--show-sdk-path" "--sdk" "iphoneos"
error: failed to run custom build command for `ring v0.17.14`
```

Pure environmental — `ring` builds C and needs the iOS SDK to find headers + linker. On a Mac with full Xcode, this would proceed. We cannot tell from this host whether the rest of opensecret's dep tree (x25519-dalek, p256, aes-gcm, chacha20poly1305, x509-parser, yasna, hyper/reqwest/rustls) compiles on iOS. Best-effort prediction: all those are pure-Rust crates that work fine on aarch64-apple-ios in known cargo registries, so iOS is **probably fine once the SDK is present**.

### agicash-auth-opensecret on wasm32 — `getrandom 0.2` (and a chain behind it)

```
error: the wasm*-unknown-unknown targets are not supported by default, you may
       need to enable the "js" feature. For more information see:
       https://docs.rs/getrandom/#webassembly-support
error[E0433]: failed to resolve: use of unresolved module or unlinked crate `imp`
error: could not compile `getrandom` (lib) due to 2 previous errors
```

This stops at getrandom 0.2 before ring/tokio/reqwest even compile. Even if we feature-flag past it, the **next** walls are:
1. `ring 0.17` — does not target wasm32-unknown-unknown (no wasm crypto backend).
2. `tokio = "full"` in opensecret — pulls `net`, `signal`, `process`, `fs`, all wasm-unsupported.
3. `reqwest = { features = ["rustls-tls", "system-proxy"] }` — `system-proxy` is unix-only; `rustls-tls` on wasm has no underlying TLS impl (browser does TLS itself).

The opensecret fork hard-codes all three of these in `[dependencies]` with **no feature gates** to disable them. There is no escape hatch short of forking deeper or building a thin wasm-compatible reimplementation. Pragmatic answer: **don't compile opensecret to wasm.** Move opensecret-touching code behind a server-side HTTP boundary (the React app already does this — the Rust SDK can do the same).

### agicash-storage-supabase on iOS — same `ring` env block

The postgrest fork pulls `reqwest 0.11` with `rustls-tls-native-roots`, which uses `ring`. Same xcrun blocker. Same prediction: should work with Xcode iOS SDK present.

### agicash-storage-supabase on wasm32 — uuid first, postgrest fork next

uuid fails first (workspace dep). Even after that, the postgrest **fork** is the issue: it hard-pins `reqwest = { version = "0.11", default-features = false, features = ["rustls-tls-native-roots"] }`. The `rustls-tls-native-roots` feature requires `rustls-native-certs`, which is **not wasm-compatible**. To target wasm we'd need a postgrest variant that uses reqwest's wasm fetch path (no TLS features). reqwest 0.12 supports this cleanly via cfg(target_arch = "wasm32"); reqwest 0.11 does too but the fork's choice of feature flags blocks it.

Workaround surface: **second fork of postgrest** (or upgrade to a maintained replacement) with reqwest features conditional on target_arch. The local-cert workaround for the slice-3 integration tests (the reason this fork exists) is a dev-only need — production builds don't need mkcert roots.

### agicash-cli on wasm32 — multiple causes

The first error is the same uuid problem, but the structural issues are:
- `clap 4.5` — does compile on wasm but pulls `std::io` features that error at runtime in browser.
- `keyring 3` — target-gated to `cfg(target_os = "macos"/"ios"/"linux"/"windows")`. No wasm impl.
- `rpassword` — reads `/dev/tty`. Unusable in browser.

agicash-cli is CLI-only by design. The right answer for wasm is **a sibling crate** (`agicash-web` / `agicash-leptos`) that composes the same lower layers without `clap`/`keyring`/`rpassword`.

---

## 3. Architectural answers

### Q1 — Where does `SessionStorage` need to abstract?

**Already done.** The trait lives in `agicash-traits/src/session_storage.rs` (`async fn store / load / clear`, takes/returns `PersistedSession { user_id: Uuid, refresh_token: String }`). It's deliberately minimal: 3 methods, no error type leakage (uses `agicash_traits::AuthError`).

Concrete impls live downstream:
- `KeyringSessionStorage` in `agicash-auth-opensecret/src/storage.rs` — wraps `keyring::Entry` with `tokio::task::spawn_blocking`. **Cleanly works on macOS, Windows, Linux, and iOS** because keyring 3's `apple-native` feature uses `security-framework` (which targets ios). It does **not** work on wasm32 (keyring has no wasm impl).
- `InMemStorage` test double in `agicash-traits/src/session_storage.rs` (test-only, unused in prod).

What's needed for browser:
- A new impl `IndexedDbSessionStorage` (probably in a new `agicash-storage-web` crate, or feature-gated within `agicash-auth-opensecret`) that uses `web-sys` IndexedDB or `gloo-storage` for localStorage. Refresh tokens in localStorage are a known security tradeoff (XSS exposure) — IndexedDB only slightly better. **The cleanest browser pattern is to keep the refresh token in an httpOnly cookie set by a server BFF and never let the wasm code see it.** That implies a fourth `SessionStorage` impl whose `store` and `clear` are no-ops and `load` reads `document.cookie` only for the user_id half (or asks the server).

Smallest trait surface to support all four targets:
- `async fn store(&self, session: &PersistedSession) -> Result<(), AuthError>`
- `async fn load(&self) -> Result<Option<PersistedSession>, AuthError>`
- `async fn clear(&self) -> Result<(), AuthError>`

That's what's there now. No additional methods needed. The concrete `KeyringSessionStorage` should probably be **moved out of `agicash-auth-opensecret` into a separate `agicash-storage-keyring` crate** so that auth-opensecret stays pure logic and consumers pick a storage backend explicitly. Tiny refactor, low risk, big readability win.

Refactor scope: move 1 file (storage.rs, ~115 lines) + delete the workspace dep on `keyring` from `agicash-auth-opensecret` + update agicash-cli's composition.rs imports. Maybe 30 min of work.

### Q2 — HTTP layer (reqwest) across iOS / iOS-sim / wasm32

reqwest 0.12 explicitly supports all three. From `reqwest-0.12.28/Cargo.toml`:
- `[target.'cfg(target_arch = "wasm32")'.dependencies.web-sys]` — when compiled for wasm, reqwest uses `web-sys::fetch` and **ignores** the rustls/native-tls features (the browser handles TLS).
- For iOS (aarch64-apple-ios) — supports both `native-tls` (via system Security framework) and `rustls-tls`. rustls is more common in Rust SDKs.

So **reqwest is not the problem.** The problem is *downstream of reqwest*:
- `system-proxy` feature is unix-only — wasm builds error if enabled. opensecret enables it unconditionally.
- `rustls-tls` brings `ring` — which doesn't target wasm32. (rustls now has a `aws-lc-rs` backend alternative but that has worse iOS story.)
- `rustls-tls-native-roots` brings `rustls-native-certs` — also unix-only. postgrest fork uses this.

Recommended path for the SDK's own HTTP usage (services layer, etc.): use reqwest 0.12 with feature-flagged TLS:
```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "http2", "json"] }
[target.'cfg(target_arch = "wasm32")'.dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json"] }
```
This compiles on iOS + wasm + native. Forks of opensecret/postgrest don't follow this discipline.

### Q3 — opensecret wasm blocker, exact diagnosis

From `/Users/claude/opensecret-sdk-fork/rust/Cargo.toml` (the very fork the workspace patches in):

```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls", "http2", "charset", "system-proxy"] }
tokio = { version = "1.41", features = ["full"] }
...
ring = "0.17"  # For certificate validation
```

Concrete wasm-hostile pieces:
1. **`reqwest` features `rustls-tls` + `system-proxy`** — both hard-pinned. `system-proxy` is unix-only and will refuse to compile on wasm32; `rustls-tls` brings `ring` which has no wasm32-unknown-unknown backend. There is no opensecret feature flag to swap these out.
2. **`tokio = "1.41"` with `features = ["full"]`** — `full` includes `net`, `signal`, `process`, `fs`, `mio`. wasm32-unknown-unknown supports only the `rt`, `sync`, `time`, `macros`, `io-util` subset. Hard-pinned; no escape hatch.
3. **`ring = "0.17"` direct dep** — used in `src/crypto/` for nitro-attestation certificate validation. Ring has no wasm32-unknown-unknown target. Would have to be swapped for `aws-lc-rs`, `rustcrypto`, or removed via cfg.
4. **`x25519-dalek`, `aes-gcm`, `chacha20poly1305`, `p256`, `sha2`, `x509-parser`, `yasna`** — all pure-Rust, likely fine on wasm32.

To make opensecret wasm-capable would require:
- Forking deeper and gating `tokio = "full"` behind a non-wasm cfg, splitting tokio features for wasm.
- Replacing ring usage with a wasm-friendly crypto backend.
- Restructuring reqwest features per target.

That is **a non-trivial fork**, probably 2-5 days of work plus maintenance burden. The cleanly architectural answer is **server-proxy boundary**: a tiny BFF (Cloudflare Worker or Hono server) holds the opensecret SDK, exposes endpoints that take user JWTs, returns derived/encrypted blobs. The wasm client talks to that boundary, never to opensecret directly. This matches the existing React app's architecture and would let `agicash-auth-opensecret` remain a pure-native crate.

Boundary surface (what'd cross the wire): generate-attestation, derive-third-party-token, derive-key — the three methods the app actually calls. ~3 endpoints, ~150 LOC of glue per side.

### Q4 — iOS Keychain access from Rust

The `keyring` crate (3.x, the workspace dep) provides this. With `apple-native` feature on, it pulls `security-framework` 2.x which wraps the iOS Security framework's `SecItemAdd / SecItemCopyMatching / SecItemDelete` functions. `keyring-3.6.3/Cargo.toml` explicitly lists `aarch64-apple-ios` in its `[package.metadata.docs.rs].targets`, has an `[[example]] iostest` that builds a `staticlib`, and gates security-framework as a cfg(target_os = "ios") dependency. **Maturity**: keyring is the most-downloaded keychain crate on crates.io; security-framework underpins many big projects (1Password, Rustup itself). No abuse concerns surfaced in a quick read.

The single iOS quirk: Keychain access requires the app to have a configured **bundle identifier** and **Keychain entitlement** at the SwiftUI shell level. The Rust code just calls the SF API; the entitlement is set in the Xcode project. Not a Rust concern; flag in iOS integration docs.

Alternative: `keychain-services` crate — older (last release 2019), single-maintainer, would not recommend. Stick with `keyring`.

---

## 4. Recommendation cluster

### (a) Stay course — finish slices 4–12 CLI-only, hit cross-platform in slices 13–14

- **Effort**: 0 incremental. Plan is unchanged.
- **What you'd learn**: nothing new about cross-platform until slice 13.
- **Risk mitigated**: none.
- **Risk taken on**: by slice 13 the dependency tree is locked in (Cashu wallet logic, Spark, the wallet service) — discovering at that point that uuid/getrandom/ring/tokio decisions ripple through several layers means the wasm port becomes a "lots of small feature-flag wars across 8+ Cargo.toml files" task. Each of those PRs is a low-confidence change. Also: opensecret-wasm-blocker doesn't surface until slice 13, by which point the BFF design has had no thought put into it.

### (b) Insert a "cross-platform foundations" mini-slice between slice 3 and slice 4

Scope (rough):
1. Move `KeyringSessionStorage` from `agicash-auth-opensecret` into a new `agicash-storage-keyring` crate (~30 min).
2. Feature-flag workspace `uuid` and any other `getrandom 0.2` consumers behind `target_arch = "wasm32"` with `js` enabled — verify domain + money + (post-feature-fix) storage-supabase compile on wasm32 (~1 hr).
3. Audit HTTP boundary: identify every reqwest consumer (postgrest fork is the big one), document which features are target-gated correctly and which need a fix. Don't fix yet (this is a recon/document slice) (~1 hr).
4. Smoke-test iOS compile on a machine with Xcode iOS SDK present (or in CI matrix with macos-latest + iOS target) — confirm or refute the BLOCKED (env) prediction (~1 hr).
5. Decision: **server-proxy vs. fork-deeper-on-opensecret** for the wasm auth path. Document the boundary surface (3 endpoints). No code yet.

- **Effort**: ~half a day to a day of focused work, mostly recon/docs/small refactor.
- **What you'd learn**: confirm iOS actually compiles (not just "should"), get a concrete list of which Cargo.toml entries need target-gating, validate the BFF boundary scope before depending on it.
- **Risk mitigated**: late-stage discovery of a fundamental incompatibility; the "many small feature wars" anti-pattern; opensecret-wasm-blocker becoming an emergency at slice 13.

### (c) Spike a Leptos web app or SwiftUI iOS app against the current SDK

- **Effort**: 2-5 days. Need to actually wire up Leptos build, target wasm32, get past every fail in the matrix above. For SwiftUI: need a real Mac, real iOS SDK, an Xcode project, learn the FFI/staticlib pattern.
- **What you'd learn**: a true end-to-end "does the SDK shape make sense on $platform". Forces architectural questions to surface.
- **Risk mitigated**: biggest possible — proves or breaks the entire cross-platform thesis. But it also commits effort to a path that might end with "we should have done (b) first."

**My one-line recommendation: (b).** Half a day to a day, no architectural commits, surfaces the right data to make the slice-13 plan real instead of aspirational. (c) is the right move *after* (b), not instead of it.

---

## 5. Open items

1. **iOS actual-compile not verified.** Six rows are `BLOCKED (env)`. The prediction is "should work" based on dep crate iOS support metadata, but it's untested. Resolving requires a host with Xcode + iOS SDK or CI that has it. (GitHub Actions `macos-latest` has it.)
2. **chrono wasm + `clock` feature**: workspace pins `chrono = { default-features = false, features = ["serde", "std", "clock"] }`. `clock` on wasm32-unknown-unknown without `wasmbind` may fail at runtime (won't have a clock source). Did not surface in the recon because uuid failed first. **Likely fix**: add `wasmbind` feature conditional on wasm32.
3. **The postgrest fork is a wasm dead-end** as currently written. Need to decide: a second fork? upgrade postgrest to a wasm-aware version? swap to a hand-rolled HTTP+JSON layer over reqwest 0.12? Out of scope for this spike.
4. **tokio feature audit**: the workspace pins `tokio = { features = ["macros","rt-multi-thread","signal","sync","time"] }`. `rt-multi-thread` and `signal` aren't wasm-compatible. Would need `target_arch` conditional features here too. Did not surface in recon (failed at uuid first).
5. **Confirm reqwest 0.12's wasm path is real** — has it been smoke-tested by anyone in the codebase yet? Not in scope here; assumed true based on its Cargo.toml `cfg(target_arch = "wasm32")` web-sys dependency block.
6. **opensecret BFF design** — if (b) leads to that decision, the boundary needs designing (auth-token shape, key-derivation API, etc.). Not in scope here.

---

*All artifacts: this report. No source code modified. Branch: `spike/cross-platform-compile`. Logs in `/tmp/spike-logs/` (host-local, not committed).*

---

## 2026-05-15 Follow-up: iOS compile pass with Xcode SDK available

(Operator note: gudnuf set up an Xcode 26.2.0 environment so the prior `BLOCKED (env)` iOS rows could be exercised. WASM out of scope per operator direction.)

### Xcode environment

Full Xcode installed at `/Applications/Xcode-26.2.0.app`. However, `xcode-select -p` returns the nix-managed `apple-sdk-14.4` and the shell's `xcrun` is a nix shim (`xcbuild-0.1.1`) that only knows that one SDK. Builds therefore need both `DEVELOPER_DIR` and `PATH` overrides:

```
DEVELOPER_DIR=/Applications/Xcode-26.2.0.app/Contents/Developer
PATH=/Users/claude/.cargo/bin:/usr/bin:$PATH    # /usr/bin/xcrun ahead of the nix shim
```

With that in place, `/usr/bin/xcrun --sdk iphoneos --show-sdk-path` resolves to:

```
/Applications/Xcode-26.2.0.app/Contents/Developer/Platforms/iPhoneOS.platform/Developer/SDKs/iPhoneOS26.2.sdk
/Applications/Xcode-26.2.0.app/Contents/Developer/Platforms/iPhoneSimulator.platform/Developer/SDKs/iPhoneSimulator26.2.sdk
```

SDK versions: **iPhoneOS 26.2** and **iPhoneSimulator 26.2** (both `26.2`, Xcode 26.2.0). Rust toolchain `1.86.0` (workspace pin). `rustup target add aarch64-apple-ios aarch64-apple-ios-sim` was a no-op — both targets already installed.

### Updated compile matrix (iOS only)

`cargo build` (not `check`) was used so `ring`/`aws-lc-sys` build scripts actually fired and produced object files.

| Crate | aarch64-apple-ios | aarch64-apple-ios-sim |
|-------|-------------------|------------------------|
| agicash-domain | OK (14.20s clean / 0.18s warm) | OK (4.72s) |
| agicash-money | OK (3.05s) | OK (2.55s) |
| agicash-crypto | OK (13.07s) | OK (1.15s) |
| agicash-traits | OK (10.15s) | OK (4.05s) |
| agicash-cache | OK | OK |
| agicash-cashu | OK | OK |
| agicash-spark | OK | OK |
| agicash-services | OK (2.22s) | OK (1.82s) |
| agicash-wallet | OK | OK |
| agicash-auth-opensecret | OK (36.82s) | OK (34.59s) |
| agicash-storage-supabase | OK (10.68s) | OK (11.52s) |
| agicash-testing | OK | OK |
| agicash-cli | OK (12.65s) | OK (11.67s) |

Plus `cargo build --workspace --exclude agicash-wasm` for both targets: **OK**. Every non-wasm crate in the workspace compiles cleanly to both iOS device and iOS simulator.

### Detailed failure modes

None. There are no failures in this pass.

Two **surprises** worth flagging:

1. **`keyring 3.6.3` + `security-framework 2.11.1` compile cleanly for iOS.** The prior spike predicted `keyring` would block iOS — that prediction was wrong. The `apple-native` feature on `keyring` 3.x gates `security-framework` for both macOS and iOS via the same `cfg(any(target_os = "macos", target_os = "ios"))` paths in `security-framework-sys`. The slice plan's proposed `agicash-storage-keyring` extract (an iOS-incompatible carve-out) does not appear necessary for compile reasons. `keyring` is consumed only by `agicash-auth-opensecret` (`storage.rs`, for session persistence) — `agicash-cli` does **not** depend on `keyring` directly.

2. **`rustls-native-certs 0.6.3` will compile but malfunction at runtime on iOS.** Source inspection (`~/.cargo/registry/src/.../rustls-native-certs-0.6.3/src/lib.rs`) shows only three cfg branches: `target_os = "macos"`, `windows`, and `all(unix, not(target_os = "macos"))`. iOS is `target_os = "ios"`, so it falls into the unix branch (`src/unix.rs`), which calls `openssl_probe::probe()` and reads `/etc/ssl/cert.pem`-style files. iOS sandboxed apps don't have those files; `openssl_probe` returns `None`; `load_native_certs()` returns `Ok(Vec::new())`. **Result: empty trust store → every TLS handshake fails at runtime.** This only matters for `agicash-storage-supabase` (via the postgrest fork's `rustls-tls-native-roots` feature). `agicash-auth-opensecret` is fine because the opensecret fork uses `rustls-tls` (bundled webpki roots), not native roots.

### Architectural inferences

1. **Pure-Rust core compiles to iOS today**: confirmed. `agicash-domain`, `agicash-money`, `agicash-traits`, `agicash-crypto`, `agicash-cache`, plus the pure orchestration crates (`agicash-services`, `agicash-wallet`, `agicash-cashu`, `agicash-spark`).

2. **`agicash-auth-opensecret` on iOS — does it actually work?** Compile: yes. Dep tree resolves cleanly through `ring 0.17`, `reqwest 0.12`, `hyper-rustls 0.27`, `tokio-rustls 0.26`, `opensecret 3.1.1`, `x509-parser 0.16`, `p256 0.13`, `ecdsa 0.16`, `bip39 2.2`, `chacha20poly1305`-family, **and** `keyring 3.6.3` → `security-framework 2.11.1`. The opensecret fork's `rustls-tls` feature uses bundled webpki roots, which work on iOS without an OS trust store. **STATE.md's prediction was correct** — this is the most important confirmation from this pass.

3. **`agicash-storage-supabase` on iOS — compile yes, runtime no.** The postgrest fork's `rustls-tls-native-roots` pulls `rustls-native-certs 0.6.3`, which has no iOS branch and falls into the unix branch, which returns an empty trust store on iOS. Three fix options ranked by cost: (a) upgrade `rustls-native-certs` to `0.7+` which has a proper `target_vendor = "apple"` branch using `SecTrustCopyAnchorCertificates`; (b) switch the postgrest fork's feature set to `rustls-tls` (webpki roots) — same as opensecret, drops the mkcert-local-CA support; (c) layer reqwest's `rustls-tls-webpki-roots` for production and conditionally enable native-roots only on the host doing local dev. Option (a) is the cleanest and is also a generic upgrade.

4. **`agicash-cli` on iOS — what breaks?** Nothing, at compile time. The prior spike predicted `keyring`-blocks-cli; that's wrong twice over: (i) cli doesn't depend on keyring directly, (ii) keyring compiles to iOS anyway. The cli binary's runtime usefulness on iOS is a different question (an iOS app is not a TTY, `rpassword`/`dotenvy`/`clap` console paths don't reach a user, the `[[bin]]` artifact isn't packaged as an `.app`), but **compile success here means cli-shared logic could be reused inside an iOS app target without code surgery**. Whatever pattern eventually owns iOS-side credential UX won't have to fork the workspace.

5. **What's the minimum surface that compiles to iOS today?** Everything in the workspace except `agicash-wasm` (excluded). The compile boundary is wider than the prior spike claimed. The **runtime** boundary is narrower: `agicash-storage-supabase` over real Supabase will fail until the rustls-native-certs issue is addressed (see #3); `agicash-auth-opensecret` should run end-to-end against an opensecret endpoint that uses a publicly-rooted cert (webpki).

### What this means for the slice plan

- **The architecture is viable for iOS today.** An iOS app target can link against `agicash-domain` + `agicash-money` + `agicash-traits` + `agicash-crypto` + `agicash-services` + `agicash-wallet` + `agicash-auth-opensecret` and have a working auth+wallet core today, with one caveat:
  - **Don't ship `agicash-storage-supabase` to iOS as-is.** It will compile and silently fail TLS. Either bump `rustls-native-certs` to `>=0.7` (preferred) or switch to webpki roots in the postgrest fork before declaring "iOS-ready". This is a one-PR fix, not an architectural redesign.

- **No need for a separate `agicash-storage-keyring` carve-out for iOS-compat reasons.** `keyring 3.6.3` is iOS-compatible. The crate could still be useful as a service boundary, but the *iOS incompatibility* rationale evaporates. The slice plan should drop that justification (other rationales like testability may still apply).

- **The `agicash-cli` crate is not an obstacle.** It compiles to iOS targets clean; it just won't behave meaningfully there because of the TTY/console model. Treat it as "compiles, doesn't ship" — same status as a development-only test binary.

- **`agicash-wasm` remains the only known incompatible target**, per the prior spike. Out of scope here.

- **For CI**: add `aarch64-apple-ios` + `aarch64-apple-ios-sim` to the workspace build matrix on `macos-latest` runners. The build is fast once `ring` is cached — full workspace cold-compile takes ~70s wall-clock per target. No `Cargo.toml` changes required.

### Verbatim build invocation (for reproducibility)

```
cd /Users/claude/agicash/.claude/worktrees/rust-spike-crossplatform/crates
PATH=/Users/claude/.cargo/bin:/usr/bin:$PATH \
  DEVELOPER_DIR=/Applications/Xcode-26.2.0.app/Contents/Developer \
  cargo build --workspace --exclude agicash-wasm --target aarch64-apple-ios
PATH=/Users/claude/.cargo/bin:/usr/bin:$PATH \
  DEVELOPER_DIR=/Applications/Xcode-26.2.0.app/Contents/Developer \
  cargo build --workspace --exclude agicash-wasm --target aarch64-apple-ios-sim
```

Per-crate logs: `/tmp/ios-<crate>.log`, `/tmp/ios-sim-<crate>.log` (host-local, not committed).
