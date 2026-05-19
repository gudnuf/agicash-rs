# `dev-sim` — one-command iOS simulator workflow

`dev-sim` brings a booted iPhone simulator to a testable state in one
idempotent invocation. It replaces the manual reinstall + re-trust dance
that follows every sim reboot or clean-slate.

```sh
nix develop -c dev-sim
```

## What it does

The script (`tools/dev/dev-sim.sh`) runs four steps in order. Each step
is idempotent — a second invocation with no changes is fast and prints
"unchanged" / "already present" for every step.

1. **Boot the target simulator.** Default is `iPhone 17`; override with
   `--device "<name>"` or `--udid <udid>`. If the sim is already booted,
   no-op.
2. **Install the mkcert root CA into the sim trust store.** Idempotency
   check matches the rootCA's SHA-256 fingerprint against the sim's
   `TrustStore.sqlite3` — modern (Xcode 26 / iOS 26) sims hold it under
   `data/private/var/protected/trustd/private/`, older layouts under
   `data/Library/Keychains/` with a `sha1` column. The script probes
   both paths + both columns. If present: no-op. If absent:
   `xcrun simctl keychain <udid> add-root-cert <rootCA.pem>`.
   This is the **permanent fix for F12** (`reqwest`/rust-tls couldn't
   validate the local Supabase HTTPS cert because the sim lacked the
   mkcert CA — `NSAllowsArbitraryLoads` only affects URLSession-side
   TLS, not the Rust client).
3. **Build + install the app incrementally.**
   - **xcframework** rebuilds only when sources under
     `bindings/swift/rust/` or `crates/agicash-ffi/` change (hash stamp
     under `bindings/swift/build/.dev-sim/xcfw.stamp`).
   - **Xcode project** regenerates only when `ios/Agicash/project.yml`
     changes.
   - **`.app` bundle** rebuilds only when the xcframework hash or any
     Swift / plist / entitlements / font file changed. Installs onto
     the sim if missing.
4. **Surgical session clear.** `DELETE FROM genp WHERE
   agrp='com.makeprisms.agicash'` against the sim's `keychain-2-debug.db`.
   Targets exactly the app's Keychain items by access group (declared in
   `Agicash.entitlements`) — leaves the installed mkcert CA, every other
   app's Keychain, and the rest of the sim state intact. The app is
   terminated first so a live process can't rewrite the row between
   delete and the next launch.

## Why surgical clear, not `simctl erase`

The historical reason for the blunt `simctl erase` was the **36-min
silent-hang on stale session** (memory `feedback_ios_sim_keychain_trap`)
— a stale `refresh_token` from a previous run sat in the sim Keychain
(which survives app uninstall) and the auth call blocked forever
waiting for OpenSecret to respond.

That root cause is now mitigated upstream by the 30-second
`withAuthTimeout` shipped in `5aed6e79` — a stale session now **fails
fast** with a timeout error, not a 36-minute hang. Surgical clear is
sufficient, and it has two big wins:

- The installed mkcert CA **persists** across iterations. No re-trust
  step ever, even after a thousand `dev-sim` invocations.
- The `.app` bundle **persists**, so step 3 is a no-op rebuild on the
  common path. Much faster inner loop.

## Flags

| Flag | Effect |
|------|--------|
| `--clean` | Full `simctl erase` of the sim, then re-install CA + app. Opt-in nuclear option for when you really do want a clean state. |
| `--device <name>` | Override target device. Default `iPhone 17`. |
| `--udid <udid>` | Override target by UDID. Wins over `--device`. |
| `--skip-build` | Skip step 3 entirely. Use when another lane has just built. |
| `--skip-clear` | Skip step 4. Use when you want to keep your current session. |

## Inside vs outside `nix develop`

- **Inside** the default agicash devshell (`nix develop`): `dev-sim` is
  on PATH as a script-bin derivation. `mkcert -CAROOT` resolves to
  `~/Library/Application Support/mkcert/rootCA.pem` because the shellHook
  resolves it.
- **Outside** the devshell: run as `nix develop -c dev-sim` (preferred)
  or directly via `bash tools/dev/dev-sim.sh`. The script itself doesn't
  require any Nix-only tooling beyond `mkcert`; everything else
  (`xcrun`, `xcodebuild`, `sqlite3`, `openssl`, `xcodegen`) is either
  system-provided or surfaced by the iOS devshell.

## Expected first run vs steady-state

**First run** (clean sim, no xcframework, no app installed): runs the
full Rust build + xcodebuild — several minutes. Mostly waiting on the
xcframework target. Subsequent steps are fast.

**Steady-state second run** (no source change, app already installed,
CA already trusted): a handful of seconds. Every step prints
"unchanged" / "already present" / "no-op" and the script returns
quickly. Only step 4 (the session clear) does any actual work, and
that's a single `DELETE` against a SQLite db.

## Fallback if `genp WHERE agrp=...` ever stops working

If a future iOS / sim version changes the Keychain layout such that
`agrp` is no longer plaintext-queryable, the cheapest fallback is:

1. `xcrun simctl uninstall <udid> com.makeprisms.agicash` (removes the
   app + its data container, including the per-app Keychain rows in
   most layouts).
2. Re-install via the same step-3 path.

This still **does not** require `simctl erase` and does **not**
invalidate the CA install (which lives in `TrustStore.sqlite3`,
separate from `keychain-2-debug.db`). The script doesn't fall back
silently — if the surgical-clear branch can't find a path, it WARNs
and tells you to re-run with `--clean`.

## What this does NOT do

- Does **not** generate the mkcert root. Run `mkcert -install` once.
- Does **not** start the local Supabase / OpenSecret stack. See
  `docs/local-stack.md`.
- Does **not** launch the app — script ends with the launch command you
  can run yourself:

```sh
xcrun simctl launch --console booted com.makeprisms.agicash
```
