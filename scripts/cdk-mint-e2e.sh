#!/usr/bin/env bash
# Stand up a local CDK mint (cdk-mintd) with the in-memory FakeWallet
# Lightning backend for money-path E2E tests.
#
# WHY: agicash-rs money flows (NUT-04 mint-quote receive, NUT-06 cashu
# swap, NUT-05 Lightning melt send) are otherwise only unit/build/static
# verified — a class of bug that let a P0 melt double-pay slip past the
# gate. This brings up a *real* Cashu mint the wallet code talks to over
# the wire via cdk's HttpClient/MintConnector, the exact path production
# uses. FakeWallet settles invoices deterministically and instantly so
# the melt round-trip (incl. the post-settle reconcile path) is testable
# without a Lightning node.
#
# The cdk-mintd version is PINNED to the `cdk` version this workspace
# depends on (crates/Cargo.lock) so the NUT wire protocol matches.
#
# Usage:
#   bash scripts/cdk-mint-e2e.sh test    # GATE: up mint + run E2E suite
#   bash scripts/cdk-mint-e2e.sh start   # install (if needed) + run; prints URL
#   bash scripts/cdk-mint-e2e.sh stop    # kill a running instance
#   bash scripts/cdk-mint-e2e.sh url     # print the URL it listens on
#   bash scripts/cdk-mint-e2e.sh status  # is it up?
#
# MONEY-PATH GATE: run `bash scripts/cdk-mint-e2e.sh test` before any
# push that touches melt / send / receive money logic. It exercises the
# NUT-04 receive, NUT-06 swap, NUT-05 melt, and the P0 melt-double-pay
# reconcile path against a real mint — coverage the standard
# `cargo test --workspace` gate does NOT provide (it has no live mint).
#
# The Rust E2E (crates/agicash-cashu/tests/cdk_mint_money_flows.rs,
# feature `cdk-mint-e2e`) reads AGICASH_TEST_MINT_URL; this script writes
# that URL to .cdk-mint-e2e.env on `start` for easy sourcing.
#
# protoc note: cdk-mintd pulls cdk-signatory which needs `protoc` at
# build time. We wrap `cargo install` in `nix shell nixpkgs#protobuf` so
# no host install is required.

set -euo pipefail

# --- config ---
CDK_VERSION="0.15.1"        # MUST match `cdk` in crates/Cargo.lock
LISTEN_HOST="127.0.0.1"
LISTEN_PORT="${CDK_MINT_PORT:-8087}"
STATE_DIR="${CDK_MINT_STATE_DIR:-$HOME/.cache/cdk-mint-e2e}"
INSTALL_ROOT="${CDK_MINT_INSTALL_ROOT:-$HOME/.cache/cdk-mint-e2e/install}"
# Prefer an explicit override, else the script's install root, else a
# previously-built copy under .claude/ (avoids a 10-min rebuild).
if [ -n "${CDK_MINTD_BIN:-}" ] && [ -x "${CDK_MINTD_BIN}" ]; then
  BIN="$CDK_MINTD_BIN"
elif [ -x "$INSTALL_ROOT/bin/cdk-mintd" ]; then
  BIN="$INSTALL_ROOT/bin/cdk-mintd"
elif [ -x "$HOME/agicash/.claude/cdk-mintd-install/bin/cdk-mintd" ]; then
  BIN="$HOME/agicash/.claude/cdk-mintd-install/bin/cdk-mintd"
else
  BIN="$INSTALL_ROOT/bin/cdk-mintd"
fi
PIDFILE="$STATE_DIR/cdk-mintd.pid"
LOGFILE="$STATE_DIR/cdk-mintd.log"
CFGFILE="$STATE_DIR/config.toml"
MINT_URL="http://${LISTEN_HOST}:${LISTEN_PORT}"
ENVFILE_DEFAULT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.cdk-mint-e2e.env"
ENVFILE="${CDK_MINT_ENVFILE:-$ENVFILE_DEFAULT}"

mkdir -p "$STATE_DIR"

ensure_binary() {
  if [ -x "$BIN" ]; then
    return 0
  fi
  echo "cdk-mintd not found; installing v${CDK_VERSION} (fakewallet,sqlite)..." >&2
  echo "(builds cdk-signatory which needs protoc — wrapped in nix shell)" >&2
  nix shell nixpkgs#protobuf -c cargo install cdk-mintd \
    --version "$CDK_VERSION" \
    --no-default-features \
    --features "fakewallet,sqlite" \
    --root "$INSTALL_ROOT"
}

write_config() {
  # FakeWallet: min/max delay 0 => invoices settle instantly and
  # deterministically. supported_units sat. mnemonic is a fixed test
  # phrase (deterministic keysets across restarts).
  cat > "$CFGFILE" <<EOF
[info]
url = "${MINT_URL}/"
listen_host = "${LISTEN_HOST}"
listen_port = ${LISTEN_PORT}
mnemonic = "test test test test test test test test test test test junk"

[info.quote_ttl]
mint_ttl = 3600
melt_ttl = 600

[info.http_cache]
backend = "memory"
ttl = 60
tti = 60

[mint_management_rpc]
enabled = false

[mint_info]
name = "agicash-rs e2e fakewallet mint"

[database]
engine = "sqlite"

[ln]
ln_backend = "fakewallet"

[fake_wallet]
supported_units = ["sat"]
fee_percent = 0.0
reserve_fee_min = 0
min_delay_time = 0
max_delay_time = 0

[limits]
max_inputs = 1000
max_outputs = 1000
EOF
}

is_up() {
  curl -fsS --max-time 2 "${MINT_URL}/v1/info" >/dev/null 2>&1
}

cmd_start() {
  if is_up; then
    echo "$MINT_URL"
    return 0
  fi
  ensure_binary
  write_config
  : > "$LOGFILE"
  # -w keeps the sqlite db + keys inside STATE_DIR.
  nohup "$BIN" -w "$STATE_DIR" --config "$CFGFILE" \
    >>"$LOGFILE" 2>&1 &
  echo $! > "$PIDFILE"
  for _ in $(seq 1 60); do
    if is_up; then
      echo "AGICASH_TEST_MINT_URL=${MINT_URL}" > "$ENVFILE"
      echo "$MINT_URL"
      return 0
    fi
    sleep 0.5
  done
  echo "cdk-mintd did not become ready in 30s; tail of log:" >&2
  tail -30 "$LOGFILE" >&2 || true
  return 1
}

cmd_stop() {
  if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    rm -f "$PIDFILE"
  fi
  pkill -f "cdk-mintd -w $STATE_DIR" 2>/dev/null || true
  rm -f "$ENVFILE"
  echo "stopped" >&2
}

# All-in-one money-path gate: bring the mint up (if needed), run the
# gated E2E suite against it, leave the mint running. This is the single
# command to run before any money-path push.
cmd_test() {
  local started_here=0
  if ! is_up; then
    cmd_start >/dev/null
    started_here=1
  fi
  echo "mint: $MINT_URL (started_here=$started_here)" >&2
  local repo
  repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  (
    cd "$repo/crates"
    AGICASH_TEST_MINT_URL="$MINT_URL" nix develop -c cargo test \
      -p agicash-cashu --features cdk-mint-e2e \
      --test cdk_mint_money_flows -- --test-threads=1
  )
}

case "${1:-start}" in
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  url)    echo "$MINT_URL" ;;
  status) if is_up; then echo "up: $MINT_URL"; else echo "down"; exit 1; fi ;;
  test)   cmd_test ;;
  *) echo "usage: $0 {start|stop|url|status|test}" >&2; exit 2 ;;
esac
