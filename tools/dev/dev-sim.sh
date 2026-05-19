#!/usr/bin/env bash
# dev-sim — bring a booted iPhone simulator to a testable state in one
# idempotent invocation.
#
# Replaces the manual reinstall + re-trust dance that follows every sim
# reboot / clean-slate. Steps:
#
#   1. Boot the target sim (default: iPhone 17) if not already booted.
#   2. Install the mkcert root CA into the sim trust store IF MISSING
#      (resolves F12: rust-tls can't validate local Supabase https without
#      this — it does NOT read NSAllowsArbitraryLoads).
#   3. Rebuild the Rust xcframework ONLY when rust sources changed.
#      Regenerate the Xcode project ONLY when project.yml changed.
#      Build + install the .app ONLY when missing or out-of-date.
#   4. Surgically clear the app's Keychain session items (genp rows where
#      agrp = com.makeprisms.agicash). Leaves the installed mkcert CA
#      and the rest of the sim state intact. The 36-min silent-hang
#      original reason for the blunt `simctl erase` is now mitigated
#      upstream by the 30s withAuthTimeout (commit 5aed6e79) — stale
#      sessions fail fast, surgical clear is sufficient.
#
# Flags:
#   --clean             Full `simctl erase` of the target sim, then
#                       re-install the CA + app. Opt-in nuclear option.
#   --device <name>     Override target device (default: "iPhone 17").
#   --udid <udid>       Override target by UDID (wins over --device).
#   --skip-build        Skip xcframework + app build/install entirely.
#                       Use after a build window in another lane.
#   --skip-clear        Don't clear the session keychain items.
#   -h, --help          Print this help.
#
# Idempotency contract: a second run with no changes should be FAST and
# print "already present" / "unchanged" for every step.
#
# External-script only. Zero iOS app source change. Zero rust change.

set -euo pipefail

# ----- locate repo root --------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ----- arg parsing -------------------------------------------------------
DEVICE_NAME="iPhone 17"
UDID=""
DO_CLEAN=0
DO_BUILD=1
DO_CLEAR=1

while [ $# -gt 0 ]; do
  case "$1" in
    --clean) DO_CLEAN=1; shift ;;
    --device) DEVICE_NAME="$2"; shift 2 ;;
    --udid) UDID="$2"; shift 2 ;;
    --skip-build) DO_BUILD=0; shift ;;
    --skip-clear) DO_CLEAR=0; shift ;;
    -h|--help)
      sed -n '2,/^set -euo/p' "$0" | sed -n '2,$p' | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "dev-sim: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# ----- logging helpers ---------------------------------------------------
log()  { printf '[dev-sim] %s\n' "$*"; }
step() { printf '\n[dev-sim] === %s ===\n' "$*"; }
warn() { printf '[dev-sim] WARN: %s\n' "$*" >&2; }
die()  { printf '[dev-sim] FATAL: %s\n' "$*" >&2; exit 1; }

# ----- 0. resolve target sim UDID ----------------------------------------
step "0. resolve target sim"
if [ -z "$UDID" ]; then
  # List by name; pick the first matching entry from the most recent
  # runtime. simctl outputs e.g.
  #   "    iPhone 17 (D8B557E3-...) (Booted)"
  UDID="$(xcrun simctl list devices "$DEVICE_NAME" \
           | awk -v name="$DEVICE_NAME" '
               $0 ~ "^-- " { runtime=$0 }
               $0 ~ name " \\(" {
                 # extract UUID inside parens
                 match($0, /[0-9A-F-]{36}/)
                 if (RSTART) print substr($0, RSTART, RLENGTH)
                 exit
               }')"
fi
[ -n "$UDID" ] || die "no simulator matching '$DEVICE_NAME' (use --device or --udid)"
log "target UDID: $UDID ($DEVICE_NAME)"

SIM_DATA_DIR="$HOME/Library/Developer/CoreSimulator/Devices/$UDID/data"
SIM_KEYCHAIN_DIR="$SIM_DATA_DIR/Library/Keychains"

# ----- --clean: nuclear reset -------------------------------------------
if [ "$DO_CLEAN" = 1 ]; then
  step "--clean: shutdown + erase $UDID"
  xcrun simctl shutdown "$UDID" 2>/dev/null || true
  xcrun simctl erase "$UDID"
  log "erased."
fi

# ----- 1. boot if not already booted -------------------------------------
step "1. boot target sim"
STATE="$(xcrun simctl list devices | awk -v u="$UDID" '
  $0 ~ u {
    # state is the last parenthesised token on the line
    n = split($0, a, /\(/)
    s = a[n]; sub(/\).*/, "", s); print s; exit
  }')"
log "current state: $STATE"
if [ "$STATE" = "Booted" ]; then
  log "already booted — no-op."
else
  log "booting…"
  xcrun simctl boot "$UDID"
  xcrun simctl bootstatus "$UDID" -b
  log "booted."
fi

# ----- 2. install mkcert root CA (idempotent) ----------------------------
step "2. mkcert root CA"
if ! command -v mkcert >/dev/null 2>&1; then
  die "mkcert not on PATH — run inside 'nix develop' (the default shell exports it)."
fi
CAROOT="$(mkcert -CAROOT)"
ROOT_CA_PEM="$CAROOT/rootCA.pem"
[ -f "$ROOT_CA_PEM" ] || die "mkcert rootCA not found at $ROOT_CA_PEM (run 'mkcert -install' once)."

# Compute the SHA-1 fingerprint of the rootCA to use as the
# already-present marker. The simulator's TrustStore.sqlite3 stores
# certificates keyed by their SHA-1 hash in the `tsettings.sha1` BLOB
# column. Match on hex(sha1) for an idempotency check that doesn't
# depend on internal Apple APIs.
ROOT_CA_SHA1="$(/usr/bin/openssl x509 -in "$ROOT_CA_PEM" -noout -fingerprint -sha1 \
                  | sed -e 's/^.*=//' -e 's/://g' \
                  | tr 'a-z' 'A-Z')"
log "rootCA SHA-1: $ROOT_CA_SHA1"

TRUSTSTORE="$SIM_KEYCHAIN_DIR/TrustStore.sqlite3"
CA_PRESENT=0
if [ -f "$TRUSTSTORE" ]; then
  # Wrap in a sub-pipe so a missing `tsettings` table doesn't kill the script.
  if HEX_LIST="$(/usr/bin/sqlite3 "$TRUSTSTORE" \
                  "SELECT upper(hex(sha1)) FROM tsettings;" 2>/dev/null)"; then
    if echo "$HEX_LIST" | grep -qx "$ROOT_CA_SHA1"; then
      CA_PRESENT=1
    fi
  fi
fi

if [ "$CA_PRESENT" = 1 ]; then
  log "mkcert CA already present in sim trust store — no-op."
else
  log "installing mkcert CA into sim trust store…"
  xcrun simctl keychain "$UDID" add-root-cert "$ROOT_CA_PEM"
  log "installed."
fi

# ----- 3. build + install ------------------------------------------------
STAMP_DIR="$ROOT/bindings/swift/build/.dev-sim"
mkdir -p "$STAMP_DIR"
XCFRAMEWORK="$ROOT/bindings/swift/build/xcframework/agicash_ffiFFI.xcframework"
APP_TRACKED_SWIFT="$ROOT/ios/Agicash/Agicash/AgicashSDK/agicash_ffi.swift"
XCODEPROJ="$ROOT/ios/Agicash/Agicash.xcodeproj"
APP_PRODUCT_DIR="$ROOT/ios/Agicash/build/Build/Products/Debug-iphonesimulator"
APP_BUNDLE="$APP_PRODUCT_DIR/Agicash.app"

# Inputs that, if changed, mean the xcframework must be rebuilt. Keep
# this list tight — anything else here = pointless rebuilds. Files only,
# not target/ output. Sort makes the hash stable across find ordering.
xcfw_input_hash() {
  {
    find "$ROOT/bindings/swift/rust" -type f \
      -not -path '*/target/*' -not -path '*/.dev-sim/*' 2>/dev/null
    find "$ROOT/crates/agicash-ffi" -type f \
      -not -path '*/target/*' 2>/dev/null
    echo "$ROOT/bindings/swift/generate-bindings.sh"
  } | sort | xargs /usr/bin/shasum -a 256 2>/dev/null | /usr/bin/shasum -a 256 | awk '{print $1}'
}

# Inputs that affect the Xcode project layout.
proj_input_hash() {
  /usr/bin/shasum -a 256 "$ROOT/ios/Agicash/project.yml" | awk '{print $1}'
}

# App-bundle freshness signal — embeds the xcframework hash + a hash of
# every .swift file under ios/Agicash/Agicash. If either changed, the
# installed .app is stale.
app_input_hash() {
  {
    echo "$1"  # current xcfw hash (passed in)
    find "$ROOT/ios/Agicash/Agicash" -type f \
      \( -name '*.swift' -o -name '*.plist' -o -name '*.entitlements' \
         -o -name '*.ttf' -o -name '*.otf' \) 2>/dev/null \
      | sort | xargs /usr/bin/shasum -a 256 2>/dev/null \
      | /usr/bin/shasum -a 256 | awk '{print $1}'
  } | /usr/bin/shasum -a 256 | awk '{print $1}'
}

if [ "$DO_BUILD" = 1 ]; then
  step "3. build + install"

  # ---- 3a. xcframework (rebuild only on input change) ----
  XCFW_STAMP="$STAMP_DIR/xcfw.stamp"
  CURR_XCFW_HASH="$(xcfw_input_hash)"
  PREV_XCFW_HASH="$( [ -f "$XCFW_STAMP" ] && cat "$XCFW_STAMP" || true )"

  if [ ! -d "$XCFRAMEWORK" ] || [ "$CURR_XCFW_HASH" != "$PREV_XCFW_HASH" ]; then
    if [ ! -d "$XCFRAMEWORK" ]; then
      log "xcframework missing — building…"
    else
      log "rust inputs changed — rebuilding xcframework…"
    fi
    (cd "$ROOT" && bash bindings/swift/generate-bindings.sh)
    echo "$CURR_XCFW_HASH" > "$XCFW_STAMP"
    log "xcframework built."
  else
    log "xcframework unchanged — skip rebuild."
  fi

  # ---- 3b. xcodegen (regen only on project.yml change) ----
  PROJ_STAMP="$STAMP_DIR/xcodeproj.stamp"
  CURR_PROJ_HASH="$(proj_input_hash)"
  PREV_PROJ_HASH="$( [ -f "$PROJ_STAMP" ] && cat "$PROJ_STAMP" || true )"

  if [ ! -d "$XCODEPROJ" ] || [ "$CURR_PROJ_HASH" != "$PREV_PROJ_HASH" ]; then
    log "regenerating Xcode project (xcodegen)…"
    (cd "$ROOT/ios/Agicash" && xcodegen generate)
    echo "$CURR_PROJ_HASH" > "$PROJ_STAMP"
  else
    log "Xcode project unchanged — skip xcodegen."
  fi

  # ---- 3c. .app build + install (only when stale or missing) ----
  APP_STAMP="$STAMP_DIR/app.stamp"
  CURR_APP_HASH="$(app_input_hash "$CURR_XCFW_HASH")"
  PREV_APP_HASH="$( [ -f "$APP_STAMP" ] && cat "$APP_STAMP" || true )"

  APP_INSTALLED=0
  if xcrun simctl listapps "$UDID" 2>/dev/null | grep -q '"com.makeprisms.agicash"'; then
    APP_INSTALLED=1
  fi

  if [ ! -d "$APP_BUNDLE" ] || [ "$CURR_APP_HASH" != "$PREV_APP_HASH" ]; then
    log "building Agicash.app for iphonesimulator (arm64)…"
    # Resolve DEVELOPER_DIR to a real Xcode (matches generate-bindings.sh
    # logic — the Nix iOS shell points DEVELOPER_DIR at the SDK).
    if XCODE_DEV_DIR="$(/usr/bin/xcode-select -p 2>/dev/null)" && [ -d "$XCODE_DEV_DIR" ]; then
      export DEVELOPER_DIR="$XCODE_DEV_DIR"
    fi
    (cd "$ROOT/ios/Agicash" && /usr/bin/xcodebuild \
       -project Agicash.xcodeproj \
       -scheme Agicash \
       -configuration Debug \
       -destination "platform=iOS Simulator,id=$UDID" \
       -derivedDataPath build \
       -quiet \
       build)
    echo "$CURR_APP_HASH" > "$APP_STAMP"

    log "installing Agicash.app on sim…"
    xcrun simctl install "$UDID" "$APP_BUNDLE"
    APP_INSTALLED=1
    log ".app installed."
  elif [ "$APP_INSTALLED" = 0 ]; then
    log ".app artifact fresh but not installed — installing…"
    xcrun simctl install "$UDID" "$APP_BUNDLE"
    APP_INSTALLED=1
  else
    log ".app unchanged + installed — skip build/install."
  fi
else
  step "3. build + install (skipped via --skip-build)"
fi

# ----- 4. surgical session clear -----------------------------------------
if [ "$DO_CLEAR" = 1 ]; then
  step "4. surgical session clear (preserves CA + app)"

  # Terminate the app first so a running process can't rewrite the
  # session row between DELETE and the next launch. Ignored if not
  # running.
  xcrun simctl terminate "$UDID" com.makeprisms.agicash 2>/dev/null || true

  KEYCHAIN_DB="$SIM_KEYCHAIN_DIR/keychain-2-debug.db"
  if [ ! -f "$KEYCHAIN_DB" ]; then
    log "no keychain db yet at $KEYCHAIN_DB — nothing to clear."
  else
    # The modern sim keychain encrypts svce/acct but leaves agrp
    # (access group) in plaintext. SessionStore declares the
    # access group "com.makeprisms.agicash" in entitlements, so this
    # one DELETE targets exactly the app's Keychain items and nothing
    # else. genp = generic-password class. We do NOT touch inet
    # (internet pw), cert, keys, or any system access group.
    BEFORE="$(/usr/bin/sqlite3 "$KEYCHAIN_DB" \
               "SELECT count(*) FROM genp WHERE agrp='com.makeprisms.agicash';" \
               2>/dev/null || echo 0)"
    if [ "$BEFORE" = "0" ]; then
      log "no app-session items present — no-op."
    else
      /usr/bin/sqlite3 "$KEYCHAIN_DB" \
        "DELETE FROM genp WHERE agrp='com.makeprisms.agicash';"
      log "cleared $BEFORE app-session item(s) from genp."
    fi
  fi
else
  step "4. session clear (skipped via --skip-clear)"
fi

# ----- summary -----------------------------------------------------------
step "summary"
log "sim:          $UDID ($DEVICE_NAME)"
log "CA installed: $( [ "$CA_PRESENT" = 1 ] && echo 'already present' || echo 'installed this run' )"
if [ "$DO_BUILD" = 1 ]; then
  log "xcframework:  $( [ "${PREV_XCFW_HASH:-}" = "$CURR_XCFW_HASH" ] && echo 'unchanged' || echo 'rebuilt' )"
  log "Xcode proj:   $( [ "${PREV_PROJ_HASH:-}" = "$CURR_PROJ_HASH" ] && echo 'unchanged' || echo 'regenerated' )"
  log "app:          $( [ "${PREV_APP_HASH:-}" = "$CURR_APP_HASH" ] && echo 'unchanged + installed' || echo 'rebuilt + installed' )"
fi
log "done. launch with: xcrun simctl launch --console booted com.makeprisms.agicash"
