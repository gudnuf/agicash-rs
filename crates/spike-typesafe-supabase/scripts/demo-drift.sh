#!/usr/bin/env bash
# Demonstrates acceptance criterion #4: a SQL migration that adds a new
# required column AND the Rust code that doesn't update fails to compile.
#
# Usage:
#   bash scripts/demo-drift.sh
#
# What it does:
#  1. Snapshots current generated.rs.
#  2. Adds a NOT NULL column to wallet.users in a temp psql session.
#  3. Re-runs codegen.
#  4. Runs `cargo build` — the test in lib.rs that constructs `NewUsers`
#     literally (with all known fields) MUST fail to compile because there's
#     a new required field.
#  5. Reverts the column and regenerates so the tree is clean again.
#
# Exits non-zero if drift was NOT caught at compile time (i.e. the codegen
# missed the new column or the literal still compiled).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT/crates"

PSQL_CMD="docker exec supabase_db_agicash psql -U postgres -v ON_ERROR_STOP=1"
GENERATED=spike-typesafe-supabase/src/generated.rs
SNAPSHOT="$(mktemp)"
DB_URL="postgres://postgres:postgres@127.0.0.1:54322/postgres"

cleanup() {
  echo
  echo "[demo-drift] reverting schema change"
  $PSQL_CMD -c "alter table wallet.users drop column if exists drift_demo_required;" >/dev/null
  echo "[demo-drift] regenerating from clean schema"
  cargo run -q -p spike-typesafe-supabase-codegen -- \
    --database-url "$DB_URL" --schema wallet --out "$GENERATED" >/dev/null
  rm -f "$SNAPSHOT"
}
trap cleanup EXIT

echo "[demo-drift] step 1: regenerate from clean schema to establish baseline"
cargo run -q -p spike-typesafe-supabase-codegen -- \
  --database-url "$DB_URL" --schema wallet --out "$GENERATED" >/dev/null
cp "$GENERATED" "$SNAPSHOT"

echo "[demo-drift] step 2: baseline build must pass"
if ! cargo build -q -p spike-typesafe-supabase 2>&1; then
  echo "[demo-drift] baseline build failed — aborting"
  exit 2
fi

echo "[demo-drift] step 3: add a NOT NULL column with no default to wallet.users"
$PSQL_CMD -c "alter table wallet.users add column drift_demo_required text not null default 'x';" >/dev/null
# Important — drop the default so the column has no column_default, forcing
# the codegen to mark it required in NewUsers.
$PSQL_CMD -c "alter table wallet.users alter column drift_demo_required drop default;" >/dev/null

echo "[demo-drift] step 4: regenerate bindings"
cargo run -q -p spike-typesafe-supabase-codegen -- \
  --database-url "$DB_URL" --schema wallet --out "$GENERATED" >/dev/null

if ! grep -q 'pub drift_demo_required: String,' "$GENERATED"; then
  echo "[demo-drift] FAIL: codegen did not pick up the new column"
  exit 3
fi

echo "[demo-drift] step 5: build must now fail (missing field in NewUsers literal)"
if cargo build -q -p spike-typesafe-supabase 2>/tmp/drift-build.log; then
  echo "[demo-drift] FAIL: build succeeded but should have failed — drift not caught at compile time"
  exit 4
fi

if grep -q 'missing field `drift_demo_required`' /tmp/drift-build.log; then
  echo
  echo "[demo-drift] PASS: drift caught at compile time:"
  grep -E 'missing field `drift_demo_required`|^error' /tmp/drift-build.log | head -8
  exit 0
fi

echo "[demo-drift] FAIL: build failed but not for the expected reason. Log:"
sed -n '1,40p' /tmp/drift-build.log
exit 5
