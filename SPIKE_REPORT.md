# Android Compile Recon — Agicash Rust SDK

Spike branch: `spike/android-compile` from `feat/rust-money-cashu` @ `52b4bf69` (slice 4 head).
Toolchain pin: workspace `crates/rust-toolchain.toml` → `1.88.0`. System default `rustc 1.95.0`. Probe never reached the toolchain — see below.

**Status: HALTED in Phase 0.** The Android NDK is not installed on this host. cargo-ndk and the three Android Rust targets are present, but a real build cannot fire without the NDK linkers and sysroots. No compile matrix data was collected. The remainder of this report documents environment state, the NDK install options, and the predictions / unknowns the operator needs to resolve before Phase 2 can run.

---

## 1. Toolchain detected

| Component | State |
|-----------|-------|
| `rustup` | Installed at `~/.cargo/bin/rustup`, not on default `$PATH`. Use `PATH=$HOME/.cargo/bin:$PATH` in build commands. |
| `rustc` (system default) | `1.95.0 (59807616e 2026-04-14)` |
| `rustc` (workspace pin) | `1.88.0` via `crates/rust-toolchain.toml` (rustup will auto-download on first build inside `crates/`). The pin's `targets` list contains only `wasm32-unknown-unknown`, so the Android stdlibs need to be added explicitly **for the 1.88.0 toolchain** once it activates (see Open Items). |
| `cargo` | `1.95.0` (system); workspace pin will provide the matching `1.88.0`. |
| Rust targets installed (system-default toolchain) | `aarch64-apple-darwin`, `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`, **`aarch64-linux-android`**, **`armv7-linux-androideabi`**, **`x86_64-linux-android`**, `wasm32-unknown-unknown`. All three Android targets already in place — `rustup target add` would be a no-op for the system toolchain. |
| `cargo-ndk` | **Installed by this spike**: `cargo-ndk 4.1.2` (also `cargo-ndk-env`, `cargo-ndk-runner`, `cargo-ndk-test`). Installation took ~62s. Confirmed working via `cargo ndk --version`. |
| Android NDK | **NOT INSTALLED.** No NDK in `~/Library/Android/sdk/ndk`, `~/.android/sdk/ndk`, `/opt/homebrew/share/android-ndk`, `/opt/homebrew/Caskroom/android-ndk`, or anywhere reachable. `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT`, `ANDROID_HOME`, `NDK_HOME` all empty. `which adb` / `which sdkmanager` → not found. `brew list --cask` → no android cask installed. |
| Android Studio.app | **Present** at `/Applications/Android Studio.app` (`CFBundleShortVersionString = 2025.1`). **Never opened.** No `~/Library/Application Support/Google/AndroidStudio*` preferences directory, no SDK manager state, no platform tools, no NDK. The `android-ndk` IDE plugin is bundled inside the Studio app but the **plugin** is not the **NDK toolchain** — the NDK has to be downloaded as a separate SDK component via the SDK Manager once Studio is first-launched. |
| Homebrew formulae for Android | None installed (`brew list | grep -i android` empty). The cask `android-ndk` exists upstream: `brew info --cask android-ndk` reports NDK r29 available, ~1.5 GB, with caveat to `export ANDROID_NDK_HOME="/opt/homebrew/share/android-ndk"`. |
| Nix Android tooling | `android-nixpkgs` (tadfisher/android-nixpkgs) appears in `~/.config/nix-config/flake.lock`, but **as a transitive input of `pika`**, not as a direct consumer in any of the operator's `home/modules/*.nix`. No package from it is installed into the user profile. There is no active Android tooling in the operator's nix configuration. |

### Probe commands attempted

```
cargo ndk -t aarch64-linux-android build -p agicash-domain
→ error: Could not find any NDK.
  note: Set the environment ANDROID_NDK_HOME to your NDK installation's root directory,
        or install the NDK using Android Studio.
```

No further builds were attempted; the toolchain stops at this gate regardless of crate. The 1.88.0 toolchain has not yet been pulled (the probe ran under the system default 1.95.0, which is sufficient to confirm cargo-ndk can find / not-find the NDK).

---

## 2. Compile matrix

**Not collected.** The NDK blocker is environmental and prevents every (crate, target) cell. Every cell would be `BLOCKED (env)` with identical reason: `cargo-ndk: Could not find any NDK`.

For the operator's planning: the matrix would have been 8 crates × 3 targets = 24 cells:

| Crate | aarch64-linux-android | armv7-linux-androideabi | x86_64-linux-android |
|-------|------------------------|--------------------------|------------------------|
| agicash-domain | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-money | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-traits | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-exchange-rate | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-auth-opensecret | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-storage-supabase | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-cashu | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |
| agicash-cli | BLOCKED (env) | BLOCKED (env) | BLOCKED (env) |

(All same reason; not reproduced 24 times above.)

---

## 3. Detailed failure modes

None observable. Only the NDK absence:

```
$ cargo ndk -t aarch64-linux-android build -p agicash-domain
error: Could not find any NDK.
note: Set the environment ANDROID_NDK_HOME to your NDK installation's root directory,
or install the NDK using Android Studio.
```

cargo-ndk searches (in order): `$ANDROID_NDK_HOME`, `$ANDROID_NDK_ROOT`, `$ANDROID_NDK_LATEST_HOME`, and well-known SDK paths under `~/Library/Android/sdk/ndk/`. None resolved.

---

## 4. Architectural inferences (predictions only — unverified)

Since no builds ran, these are **predictions** based on source-tree inspection plus the iOS spike's findings on the same workspace. Confidence noted per item.

### Q1 — Pure-Rust core compiles to Android?

**Prediction: yes, high confidence.** `agicash-domain`, `agicash-money`, `agicash-traits` are pure-Rust crates with workspace deps `serde`, `serde_json`, `chrono`, `thiserror`, `uuid`, `rust_decimal`, `async-trait`. All of those crates publish working `aarch64-linux-android` builds and have no `cfg(unix)` / `cfg(macos)` gates that would exclude Android. The same crates were `OK` on `aarch64-apple-ios` in the prior spike with no source changes; Android target adds no new compile-time requirements for pure-Rust code. `agicash-exchange-rate` is `reqwest 0.12 + rustls-tls + serde` — also expected `OK` (reqwest 0.12 has a `target_os = "android"` cfg path that uses standard rustls, no Apple/Linux-specific shims).

### Q2 — `agicash-auth-opensecret` on Android: ring, reqwest, tokio, opensecret

Three things to verify once NDK is in place:

1. **`ring 0.17` on Android.** `ring` officially supports `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android` in its CI matrix. It uses NDK clang (`aarch64-linux-android21-clang`) for the C/asm bits; cargo-ndk wires this up automatically. **Prediction: OK.** Higher confidence than iOS because Android NDK is simpler than xcrun / DEVELOPER_DIR — once cargo-ndk has the NDK path, the C build "just works".

2. **`reqwest 0.12 + tokio + rustls`.** reqwest 0.12 has explicit Android support and uses standard rustls (which uses ring) for TLS. tokio with `["macros","rt-multi-thread","sync","time","signal"]` features compiles to Android (signal is the only marginal item — on Android, `tokio::signal` works since Android is Linux-flavored). **Prediction: OK.**

3. **`opensecret 3.1.1`** (workspace patches in `~/opensecret-sdk-fork/rust`). The fork hard-pins `reqwest = { features = [..., "system-proxy"] }` and `tokio = { features = ["full"] }`. On Android both *should* compile — `system-proxy` is gated `cfg(unix)` which Android matches, and `tokio = "full"` is supported on Android (it's a unix Linux target). **Prediction: OK** — looser than iOS because Android being Linux-flavored matches more `cfg(unix)` branches without surgery. (Contrast: this was the wasm dead-end exactly because wasm matches *none* of those gates.)

4. **OpenSecret JWT / key storage story.** OpenSecret's session storage on iOS goes through `keyring` → `apple-native` → `security-framework` → iOS Keychain. On Android, the `keyring` 3.x crate has **no native Android backend**. Its target-cfg is:

   ```toml
   # from ~/.cargo/registry/src/.../keyring-3.6.3/Cargo.toml (predicted layout)
   apple-native       = ["security-framework"]   # macos + ios
   windows-native     = [...]                     # windows
   linux-native-sync-persistent = [...]           # linux secret-service via dbus
   crypto-rust        = [...]                     # encrypted-file fallback
   ```

   The workspace dep is `keyring = { features = ["apple-native", "windows-native", "linux-native-sync-persistent", "crypto-rust"] }`. **On Android `target_os = "android"` matches NONE of these**: `apple-native` needs macos/ios, `linux-native` requires dbus + libsecret (not on Android), `crypto-rust` provides a `keyutils`-or-file fallback that may or may not target Android. **Prediction: keyring will fail to provide a working backend on Android** — same shape as the wasm story, but for a different reason. The Rust code may still compile (target-gated impls compile to no-ops), but `Entry::new(...)` will fail at runtime with `PlatformFailure`.

   **The realistic Android path is the same pattern the iOS scaffold used: Kotlin/Java side owns token storage** (Android `EncryptedSharedPreferences` or `Android Keystore`), and the Rust `SessionStorage` trait gets a JNI-backed impl that delegates through to the Kotlin code. This mirrors the iOS scaffold's "Swift owns the Keychain, Rust gets a thin SessionStorage trait impl". The `SessionStorage` trait already exists in `agicash-traits/src/session_storage.rs`; an `agicash-storage-android-jni` impl would be the symmetric counterpart to the proposed (iOS-side) JNI-style native shim.

### Q3 — `agicash-storage-supabase` on Android

Same `rustls-native-certs 0.6.3` pitfall as iOS, but with a **different runtime outcome**.

- **Compile: likely OK.** rustls-native-certs 0.6.3's `cfg(unix)` branch matches Android (since Android `target_os = "android"` also matches the broader `unix` family in some cfg gates, depending on how the crate writes them). Source inspection on iOS revealed three branches: macos / windows / `all(unix, not(target_os = "macos"))`. Android falls into the unix branch.
- **Runtime: ambiguous.** The unix branch reads `/etc/ssl/cert.pem` via `openssl_probe`. **Android does not have `/etc/ssl/cert.pem`.** Android uses `/system/etc/security/cacerts/` for CA certs, accessed by the framework, not by reading files directly (sandboxed apps cannot read `/system/etc/security/cacerts/` in most configurations anyway). `openssl_probe` returns `None`; `load_native_certs()` returns `Ok(Vec::new())`. **Same failure mode as iOS: empty trust store → TLS handshakes fail.**
- **Fix paths**, ranked by cost (same as iOS spike Section 3 #3):
  - (a) bump `rustls-native-certs` to `>=0.7` — has a real Android branch using Android's certificate store via JNI.
  - (b) switch the postgrest fork's feature set to `rustls-tls` (webpki bundled roots) — works on Android out of the box, drops mkcert support.
  - (c) layer reqwest's `rustls-tls-webpki-roots` for production and conditionally enable native-roots only on the dev host.

  All three are workspace-level fixes, not Android-specific. The iOS spike already flagged this as a one-PR change. **Android does not add a new fix** — it shares iOS's fix.

### Q4 — Minimum subset that compiles to Android today?

Predicted, not verified:
- **Definitely OK** (pure-Rust + standard deps): `agicash-domain`, `agicash-money`, `agicash-traits`, `agicash-exchange-rate`, `agicash-cache`, `agicash-crypto`, `agicash-services`, `agicash-wallet`, `agicash-cashu`, `agicash-spark`, `agicash-testing`.
- **OK at compile, broken at runtime**: `agicash-storage-supabase` (TLS cert store empty, same as iOS).
- **OK at compile, broken at session-storage runtime**: `agicash-auth-opensecret` (`keyring` has no Android backend; the rest of the crate compiles).
- **Compiles but useless**: `agicash-cli` (no TTY on Android; `rpassword`, `clap` console paths, `dotenvy` are nonsense in an Android app — same status as on iOS, "compiles, doesn't ship").
- **Definitely NO**: `agicash-wasm` (excluded per spike scope; not an Android concern).

In short: **the same shape as iOS**. The compile boundary is wide; the runtime boundary is narrower; the auth/storage gaps require a JNI-side helper that mirrors the iOS Swift-side helper.

### Q5 — Keystore / Keychain abstraction

iOS uses `keyring` → `apple-native` → Apple Security framework. Android has **no equivalent first-class Rust crate** for the Android Keystore that the spike could find pre-blocked. Options:

1. **`keyring`'s `crypto-rust` feature** as a software-only fallback (encrypts a refresh token to disk with a software-managed key). Not hardware-backed; weakest credential security; probably acceptable for a v1.
2. **`keyutils`-style kernel keyring** — irrelevant for Android apps (kernel keyring exists, but Android sandboxes apps away from it).
3. **The "right" answer matching the iOS scaffold pattern: a JNI-backed `SessionStorage` impl.** The Kotlin side calls Android Keystore + `EncryptedSharedPreferences` (Jetpack Security) and exposes `store(token: String)` / `load(): String?` / `clear()` over JNI. Rust impl wraps the JNI calls in `agicash_traits::SessionStorage`. This is the same architecture as the iOS plan ("Swift owns token storage; Rust gets a thin SessionStorage impl"). It avoids fighting `keyring`'s Android limitations entirely.
4. **`android-native-keyring-store` crate** (saw it in the nix store as a transitive crate source — `0qbnr...crate-android-native-keyring-store-0.5.0.tar.gz`). Worth a 30-min look during a future Android phase. If it provides a working Android Keystore backend it could short-circuit option 3. **Did not verify** — the nix store had a `.tar.gz.drv` artifact, not an unpacked source, and it's a transitive build dep of something else in nix-managed Rust, not an installed library.

**Recommended path: JNI-backed `SessionStorage` impl** (option 3). It's symmetric with the iOS plan, requires zero changes to the existing `agicash-traits::SessionStorage` trait, and offloads the credential security to Android's mature Jetpack Security library. Effort estimate (once iOS scaffold is stable): ~1 day for the Kotlin side + JNI shim + Rust impl.

### Q6 — Comparison to iOS findings

| Dimension | iOS (verified) | Android (predicted) |
|-----------|----------------|----------------------|
| Pure-Rust core compiles? | YES | YES (high confidence) |
| `ring 0.17` builds? | YES (with Xcode SDK + xcrun) | YES (with NDK clang via cargo-ndk) |
| `reqwest 0.12 + rustls` works? | YES | YES |
| `keyring 3.6.3` has a working native backend? | YES (`apple-native` → Security framework) | **NO** — Android falls outside all `keyring` features |
| `rustls-native-certs 0.6.3` runtime? | **BROKEN** (empty trust store on iOS) | **BROKEN** (empty trust store on Android — `/etc/ssl/cert.pem` not present) |
| Toolchain quirks | Need `DEVELOPER_DIR` + non-nix `xcrun` on PATH | Need `ANDROID_NDK_HOME` + cargo-ndk; should be a single env var, no PATH dance |
| Bundle/entitlements (out-of-Rust glue) | Xcode bundle id + Keychain entitlement | Android Manifest permissions + JNI library packaging |
| Most-disruptive single fix | bump `rustls-native-certs` to ≥0.7 OR switch postgrest fork to webpki roots | **same fix** — Android shares this |
| Net new Android-only blocker | n/a | **`keyring` has no Android backend → need a JNI-side credential helper** |

**Bottom line**: Android findings are **qualitatively similar to iOS** with one extra item (the `keyring` gap). The dep-version bumps that fix iOS also fix Android. The "Swift owns Keychain" pattern from the iOS scaffold extends naturally to "Kotlin owns Keystore" on Android. **No surprise NDK-linker-quirks or target-triple chaos predicted** — cargo-ndk handles all of that. The main unknown is whether the workspace's `keyring` workspace-dep features compile cleanly to Android when none of them target Android (it may produce a runtime panic at session-storage init time, but should not fail compile).

---

## 5. Recommendation

**(b) Mirror iOS Phase 1: scaffold an `agicash-android` Compose app + `bindings/kotlin/rust/` wrapper crate, once iOS Phase 1 is stable.**

But not now — **defer Android start until the operator chooses to install an NDK** (any of the three install paths below) **and** the iOS scaffold has produced a stable JNI-style FFI pattern that the Android wrapper can mirror.

Effort estimate (when work begins):
- Phase 0 (this report) re-run with NDK present: ~30 min, will produce a real compile matrix.
- Phase 1 (mirror iOS scaffold): ~2-3 days for an `agicash-android` Compose app skeleton, JNI binding crate, basic auth flow. Most surface area is identical to the iOS scaffold — copy structure, swap Swift for Kotlin, swap Security framework for Android Keystore.
- Phase 1.5 (JNI `SessionStorage`): ~1 day, see Q5.
- Risk: low, since iOS has cut the path. Main wildcard is JNI ergonomics (`jni` crate vs `robusta_jni` vs `uniffi`) — the operator's preference will set the pattern.

Why not (a) "defer entirely": the data above shows Android is not architecturally harder than iOS. Same fix list, plus one extra (the JNI session-storage). Deferring "until after iOS v1 ships" already aligns with the iOS phasing; no reason to push Android further out.

Why not (c) "block on dep fix": the dep fixes (`rustls-native-certs` bump, postgrest fork webpki) are not Android-specific. They're already on the iOS list. Calling them out as an Android-blocker would double-count.

### Install paths for the operator (cleanest first)

1. **Homebrew cask** — `brew install --cask android-ndk` (NDK r29, ~1.5 GB). Sets up `/opt/homebrew/share/android-ndk` with `ndk-build` symlinks. Operator needs to `export ANDROID_NDK_HOME="/opt/homebrew/share/android-ndk"` (this can go in the nix-managed `shell.nix`'s `sessionVariables`). **Cleanest because it's a single command, no GUI, and integrates with the operator's existing brew-managed casks.**
2. **Android Studio first-run + NDK via SDK Manager**. Open `/Applications/Android Studio.app`, accept the first-run wizard, install Android SDK (default location `~/Library/Android/sdk`), then SDK Manager → SDK Tools → NDK (Side by side). ~2-5 GB total because Studio also wants the platform SDK, build-tools, emulator stubs, etc. Heavier; only worth it if the operator plans to use Android Studio interactively for the Compose app.
3. **Nix** via `android-nixpkgs`. The flake input is already there (transitively); a `home/modules/android.nix` could add `androidSdk.ndkBundle` to the user profile. Most "in style" for this host (nix-darwin everywhere else) but probably the slowest to set up cleanly, and `tadfisher/android-nixpkgs` SDK manager flow is finicky compared to the brew cask. **Worth doing eventually for reproducibility, but not for an unblock-the-spike fast path.**

Recommended for fastest unblock: **path #1 (homebrew cask)**.

---

## 6. Open items

1. **NDK not installed** — the spike halt condition. Operator must pick an install path (see Section 5). Once NDK is at `$ANDROID_NDK_HOME`, re-running this spike will take ~10 minutes per target and produce a real compile matrix.

2. **Toolchain pin behavior on first Android build**. The workspace pins `1.88.0` with `targets = ["wasm32-unknown-unknown"]`. The first Android build will trigger rustup to download the `1.88.0` toolchain, but **the Android stdlibs are added to the system default (`1.95.0`), not to `1.88.0`**. Operator will need: `rustup target add --toolchain 1.88.0 aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`. The iOS spike did not report this issue, suggesting the iOS targets were added to `1.86.0` (the iOS-era pin) at some earlier point. Same fix shape.

3. **`keyring` Android-backend ambiguity**. Did not verify whether `keyring 3.6.3` produces a *compile error* or a *runtime panic* on Android. Source inspection of `keyring-3/src/lib.rs` would resolve this in 5 minutes once the NDK is available and a compile is possible. If compile-error: the workspace `keyring` dep needs an `Android` cfg branch (probably swap to `crypto-rust` only on Android). If runtime-only: nothing to fix at the workspace level; just need the JNI helper before runtime exercise.

4. **`agicash-cli`'s `rpassword` on Android**. The `rpassword` crate is `cfg(unix)`-gated. Android is unix; will compile. The interactive prompts will fail at runtime since Android has no stdin TTY by default. Same status as iOS (compiles, useless).

5. **`uniffi` vs `jni` vs `robusta_jni`** for the FFI bindings. The iOS spike said "Swift owns the Keychain"; the corresponding Android decision (Kotlin owns the Keystore) needs a binding generator pick. `uniffi` (Mozilla, used by Firefox iOS/Android) is the strongest candidate because it generates **both** Swift and Kotlin from a single UDL/proc-macro source — letting iOS and Android share the binding generator. Worth a separate spike before Phase 1 of either platform starts.

6. **CI matrix.** Adding `aarch64-linux-android` builds to GitHub Actions needs (a) the `ubuntu-latest` runner with NDK preinstalled (it has NDK r25c by default; r29 needs manual install), (b) `cargo install cargo-ndk` in the workflow, (c) the same `rustup target add --toolchain 1.88.0` step. Estimated ~2 minutes of CI time per target once the cache warms. Out of scope for this spike but trivial to add when Android Phase 1 begins.

7. **No actual Android compile verification.** Every prediction in Section 4 is source-tree inspection plus pattern-matching to the iOS spike's findings. The actual compile matrix needs the NDK present.

---

*Branch: `spike/android-compile`. No source code modified. `cargo-ndk 4.1.2` installed into `~/.cargo/bin` (toolchain change, retained — useful regardless of Android decision). No production Cargo.toml tweaks attempted (Phase 2 never started).*
