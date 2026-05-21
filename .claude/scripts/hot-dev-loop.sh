#!/usr/bin/env bash
# hot-dev loop: every 5 min, fetch agicash-rs/master, rebuild + reinstall
# fresh APK on the emulator (if running) and refresh the Leptos wasm bundle.
# iOS rebuild logged but not auto-installed (sim install needs xcodebuild
# build-for-testing + simctl install which is workspace-specific).
#
# Watch with: tmux a -t hot-dev
# Stop with:  tmux kill-window -t hot-dev (or Ctrl-C if attached)

set -u
cd "$HOME/agicash"

WORKTREE="$HOME/agicash/.claude/worktrees/hot-dev-master"
INTERVAL_SEC=300
LAST_SHA=""

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }

mkdir -p "$(dirname "$WORKTREE")"
if [ ! -d "$WORKTREE" ]; then
  log "first run: creating worktree at $WORKTREE"
  git fetch agicash-rs >/dev/null 2>&1
  git worktree add --detach "$WORKTREE" agicash-rs/master
fi

log "hot-dev loop started; polling every ${INTERVAL_SEC}s"
log "worktree:   $WORKTREE"

while true; do
  if ! git fetch agicash-rs 2>/dev/null; then
    log "git fetch failed — will retry next tick"
    sleep "$INTERVAL_SEC"
    continue
  fi
  CURRENT_SHA=$(git rev-parse agicash-rs/master)

  if [ "$CURRENT_SHA" = "$LAST_SHA" ]; then
    log "no change (master @ ${CURRENT_SHA:0:8}) — sleeping ${INTERVAL_SEC}s"
    sleep "$INTERVAL_SEC"
    continue
  fi

  log "master moved: ${LAST_SHA:0:8} → ${CURRENT_SHA:0:8}"
  (cd "$WORKTREE" && git fetch agicash-rs >/dev/null 2>&1 && git reset --hard "$CURRENT_SHA" >/dev/null)

  # ── Android ──────────────────────────────────────────────────────────
  EMULATOR_UP=false
  if adb devices 2>/dev/null | grep -qE '^emulator-[0-9]+\s+device$'; then
    EMULATOR_UP=true
  fi
  if $EMULATOR_UP; then
    log "android: regenerating kotlin bindings"
    if (cd "$WORKTREE" && nix develop .#android -c ./bindings/kotlin/generate-bindings.sh > /tmp/hot-dev-kotlin.log 2>&1); then
      log "android: bindings ok, running gradle assembleDebug"
      if (cd "$WORKTREE/android/Agicash" && nix develop "$HOME/agicash#android" -c ./gradlew assembleDebug > /tmp/hot-dev-gradle.log 2>&1); then
        APK="$WORKTREE/android/Agicash/app/build/outputs/apk/debug/app-debug.apk"
        if [ -f "$APK" ]; then
          if adb install -r "$APK" > /tmp/hot-dev-adb.log 2>&1; then
            log "android: ✓ installed $(du -h "$APK" | cut -f1)"
          else
            log "android: ✗ adb install failed (see /tmp/hot-dev-adb.log)"
          fi
        else
          log "android: ✗ no APK at $APK"
        fi
      else
        log "android: ✗ gradle build failed (see /tmp/hot-dev-gradle.log)"
      fi
    else
      log "android: ✗ bindings regen failed (see /tmp/hot-dev-kotlin.log)"
    fi
  else
    log "android: emulator not running — skipping"
  fi

  # ── Leptos PWA ───────────────────────────────────────────────────────
  log "leptos: building wasm bundle"
  if (cd "$WORKTREE" && nix develop .#wasm -c bash -c "cd crates/agicash-web-leptos && wasm-pack build --target web --out-dir pkg --dev" > /tmp/hot-dev-leptos.log 2>&1); then
    log "leptos: ✓ wasm-pack built (operator: refresh browser)"
  else
    log "leptos: ✗ wasm-pack build failed (see /tmp/hot-dev-leptos.log)"
  fi

  # ── iOS xcframework (notify only — sim install is manual) ────────────
  if xcrun simctl list devices booted 2>/dev/null | grep -qE '\(Booted\)$'; then
    log "ios: sim booted — regenerating xcframework"
    if (cd "$WORKTREE" && nix develop .#ios -c bindings/swift/generate-bindings.sh > /tmp/hot-dev-swift.log 2>&1); then
      log "ios: ✓ xcframework rebuilt (operator: rebuild + install via Xcode)"
    else
      log "ios: ✗ bindings regen failed (see /tmp/hot-dev-swift.log)"
    fi
  fi

  log "tick done — sleeping ${INTERVAL_SEC}s"
  LAST_SHA="$CURRENT_SHA"
  sleep "$INTERVAL_SEC"
done
