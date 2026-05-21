#!/usr/bin/env bash
# hot-dev-rebuild.sh — fired by watchexec when source files change.
# Inspects WATCHEXEC_* envs to decide which platform builds to run.
# Each platform's build is gated on whether its sources actually changed.
set -u
cd "$HOME/agicash"

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }

# watchexec sets these to newline-separated paths; aggregate to one list.
CHANGED=$(printf '%s\n%s\n%s\n%s\n%s' \
  "${WATCHEXEC_CREATED_PATH:-}" \
  "${WATCHEXEC_WRITTEN_PATH:-}" \
  "${WATCHEXEC_REMOVED_PATH:-}" \
  "${WATCHEXEC_RENAMED_PATH:-}" \
  "${WATCHEXEC_META_CHANGED_PATH:-}" \
  | grep -v '^$' | sort -u)

if [ -z "$CHANGED" ]; then
  log "(initial wake or no change paths in env — nothing to do; awaiting real edits)"
  exit 0
fi

log "change burst — paths:"
echo "$CHANGED" | sed 's/^/    /' | head -10
[ "$(echo "$CHANGED" | wc -l)" -gt 10 ] && log "    ... ($(echo "$CHANGED" | wc -l) total)"

RUST_CHANGED=false
ANDROID_KT_CHANGED=false
IOS_SWIFT_CHANGED=false
LEPTOS_CHANGED=false

while IFS= read -r path; do
  case "$path" in
    "$HOME/agicash/crates/agicash-web-leptos/"*)
      LEPTOS_CHANGED=true
      RUST_CHANGED=true
      ;;
    "$HOME/agicash/crates/"*)
      RUST_CHANGED=true
      LEPTOS_CHANGED=true   # leptos transitively pulls most crates
      ;;
    "$HOME/agicash/bindings/"*)
      RUST_CHANGED=true
      ;;
    "$HOME/agicash/android/Agicash/app/src/"*)
      ANDROID_KT_CHANGED=true
      ;;
    "$HOME/agicash/ios/Agicash/Agicash/"*)
      IOS_SWIFT_CHANGED=true
      ;;
    "$HOME/agicash/nix/"*)
      RUST_CHANGED=true
      LEPTOS_CHANGED=true
      ;;
  esac
done <<< "$CHANGED"

# ── Android ──────────────────────────────────────────────────────────────
EMULATOR_UP=false
if adb devices 2>/dev/null | grep -qE '^emulator-[0-9]+\s+device$'; then
  EMULATOR_UP=true
fi

if $EMULATOR_UP && ( $RUST_CHANGED || $ANDROID_KT_CHANGED ); then
  if $RUST_CHANGED; then
    log "android: regenerating kotlin bindings (rust changed)"
    if ! nix develop .#android -c ./bindings/kotlin/generate-bindings.sh > /tmp/hot-dev-kotlin.log 2>&1; then
      log "android: ✗ bindings regen failed (tail /tmp/hot-dev-kotlin.log)"
      tail -5 /tmp/hot-dev-kotlin.log | sed 's/^/    /'
    else
      log "android: ✓ bindings regen ok"
    fi
  fi
  log "android: gradle assembleDebug"
  if ! (cd android/Agicash && nix develop "$HOME/agicash#android" -c ./gradlew assembleDebug > /tmp/hot-dev-gradle.log 2>&1); then
    log "android: ✗ gradle failed (tail /tmp/hot-dev-gradle.log)"
    tail -8 /tmp/hot-dev-gradle.log | sed 's/^/    /'
  else
    APK="android/Agicash/app/build/outputs/apk/debug/app-debug.apk"
    if [ -f "$APK" ]; then
      if adb install -r "$APK" > /tmp/hot-dev-adb.log 2>&1; then
        log "android: ✓ installed (size $(du -h "$APK" | cut -f1))"
      else
        log "android: ✗ adb install failed"
        tail -3 /tmp/hot-dev-adb.log | sed 's/^/    /'
      fi
    fi
  fi
elif ! $EMULATOR_UP && ( $RUST_CHANGED || $ANDROID_KT_CHANGED ); then
  log "android: skipped (emulator not running)"
fi

# ── Leptos PWA ───────────────────────────────────────────────────────────
if $LEPTOS_CHANGED; then
  log "leptos: wasm-pack build --target web --dev"
  if ! nix develop .#wasm -c bash -c "cd crates/agicash-web-leptos && wasm-pack build --target web --out-dir pkg --dev" > /tmp/hot-dev-leptos.log 2>&1; then
    log "leptos: ✗ wasm-pack failed (tail /tmp/hot-dev-leptos.log)"
    tail -8 /tmp/hot-dev-leptos.log | sed 's/^/    /'
  else
    log "leptos: ✓ wasm bundle built (operator: refresh browser)"
  fi
fi

# ── iOS xcframework (notify + regen; sim install left to operator) ───────
SIM_UP=false
if xcrun simctl list devices booted 2>/dev/null | grep -qE '\(Booted\)$'; then
  SIM_UP=true
fi

if $SIM_UP && $RUST_CHANGED; then
  log "ios: regenerating swift xcframework (rust changed)"
  if ! nix develop .#ios -c bindings/swift/generate-bindings.sh > /tmp/hot-dev-swift.log 2>&1; then
    log "ios: ✗ xcframework regen failed (tail /tmp/hot-dev-swift.log)"
    tail -5 /tmp/hot-dev-swift.log | sed 's/^/    /'
  else
    log "ios: ✓ xcframework rebuilt (operator: rebuild/install via Xcode)"
  fi
elif $SIM_UP && $IOS_SWIFT_CHANGED; then
  log "ios: swift-only change — Xcode rebuild needed (no CLI install path)"
fi

log "tick done"
