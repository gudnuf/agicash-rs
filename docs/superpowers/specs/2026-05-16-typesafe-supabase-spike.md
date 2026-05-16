# Typesafe Supabase storage — spike report

**Branch:** `spike/typesafe-supabase`
**Date:** 2026-05-16
**Author:** spike-typesafe-supabase
**Status:** spike complete — recommendation: custom codegen ("Path C") on top of the existing `postgrest` runtime.

## Problem statement

`agicash-storage-supabase` today uses raw strings for every table/RPC name (`client.from("accounts").eq("state", "active")`, `client.rpc("upsert_user_with_accounts", body)`) and hand-written `serde` structs in `agicash-domain` and `agicash-traits` that happen to match the SQL schema. Schema drift is only caught at runtime, usually by integration tests. We want compile-time type safety against `supabase/migrations/*.sql` for:

1. Table names
2. Column names + column types (incl. enums, FKs, nullability)
3. RPC names
4. RPC input arg names + types + nullability
5. RPC return types

…without giving up Supabase (PostgREST runtime, RLS, the existing `postgrest` crate transport).

## TL;DR

- **Recommendation:** custom codegen tool that reads migrations into a temp Postgres (we already run one for local dev — `supabase_db_agicash`, port 54322), introspects `information_schema` + `pg_catalog`, and emits `crates/agicash-storage-supabase/src/generated.rs` containing typed `tables::*`, `rpcs::*`, `enums::*`, `composites::*` modules. The `postgrest::Postgrest` client stays as the runtime; the typed wrappers just constrain which table/column/arg names are spellable and which payload shapes are valid.
- **Working PoC:** under `crates/spike-typesafe-supabase/` in this worktree. 200-line codegen tool, 1900-line generated.rs covering all 15 tables + 40 RPCs + 17 enums + 17 composites in `wallet`, all 4 acceptance criteria proven (incl. drift detection — see `scripts/demo-drift.sh`).
- **Productionization effort:** **M** (1.5–2.5 weeks). Breakdown below.
- **Fallback:** keep doing what we do today; layer the simplest typed-name constants (Path D in the matrix below) as a stop-gap. Zero of the gotchas listed below apply but it only covers ~30% of what we want.

## Inventory matrix

Legend:
- **Covers tables / RPCs:** does the tool give us typed Row/Insert/Arg/Return shapes for those things?
- **Source of truth:** what does the codegen consume? Closer to "migrations" = better fit for the hard constraint.
- **PostgREST compat:** does the runtime still go through the existing `postgrest` crate / RLS?
- **Build cost:** how heavy is the codegen step? (Local dev iteration speed.)
- **Runtime cost:** does the runtime change vs. today?
- **Maintained:** is the project alive in 2026?

| Option | Covers tables | Covers RPCs | Source of truth | PostgREST compat | Build cost | Runtime cost | Maintained | Fit for our constraints |
|---|---|---|---|---|---|---|---|---|
| **A. sqlx `query!` macros** | yes (per query) | no (would need `query!("select wallet.fn(...)")`, but loses composite/array typing) | live DB (`SQLX_OFFLINE_DIR` JSON cache from `cargo sqlx prepare`) | NO — direct DB, replaces postgrest runtime | high first build; cached in CI | replaces postgrest with sqlx pool, RLS via `SET LOCAL request.jwt.claims` | yes (active) | **No** — violates "stay with Supabase REST", drops RLS surface |
| **B. cornucopia** | yes (struct per query) | partial — accepts plain `select wallet.fn(...)` but no native composite/array support in 2026; arrays of composites are emitted as `Vec<serde_json::Value>` | `.sql` files in repo, compiled against a Docker Postgres | NO — direct postgres client | medium | replaces postgrest with tokio-postgres | yes | **No** — same reason as A, plus weaker composite story |
| **C. Custom codegen (this spike)** | yes | yes (incl. composite-arg/return) | `supabase/migrations/*.sql` via a temp Postgres (we already have one) | YES — postgrest crate untouched | low (one binary, runs in <1s vs. local DB) | unchanged (postgrest) | we own it | **Yes** — covers every constraint |
| **D. Hand-rolled name constants** (`const USERS_TABLE: &str = "users"`) plus existing structs | partial (table/column names only) | partial (RPC name only) | hand-written, drifts from migrations | yes | zero | unchanged | n/a | **Partial fallback** — beats nothing, but doesn't catch column-type, RPC-arg, or RPC-return drift |
| **E. diesel** | yes (schema.rs) | weak (functions can be declared but no codegen, no composite/array, ORM-shaped) | live DB or migrations | NO — diesel uses its own pg connection | high | replaces postgrest | yes | **No** |
| **F. sea-orm-cli generate entity** | yes (entities) | no | live DB | NO — replaces postgrest with sqlx | high | replaces postgrest | yes | **No** |
| **G. `supabase-rs` / `postgrest-typed` (crates.io community)** | no mature option — `supabase-rs` (1.2k downloads, last release 2024) is auth + storage-bucket only; nothing typed for PostgREST | n/a | n/a | n/a | n/a | n/a | dormant | **No** — there is no community winner here yet |

## What I built (PoC)

Two crates in this worktree:

### `crates/spike-typesafe-supabase-codegen/`

A standalone binary, ~430 LOC, that takes `--database-url --schema --out` and emits a single Rust file. Connects with the sync `postgres` crate (no async runtime needed for codegen). Three queries do the heavy lifting:

1. `pg_type t JOIN pg_enum e` — enum types + variant labels
2. `pg_type t JOIN pg_class c JOIN pg_attribute a` (where `t.typtype = 'c' AND c.relkind = 'c'`) — composite types' field names + Postgres types
3. `information_schema.columns` per table — column names, `pg_catalog.format_type`, `udt_schema`/`udt_name` (the cleanest way to distinguish builtin scalars from user-defined types), nullability, has-default
4. `pg_proc + pg_get_function_arguments + pg_get_function_result` — function signatures including DEFAULT markers

Run against the running local Supabase:

```bash
cargo run -p spike-typesafe-supabase-codegen -- \
  --database-url "postgres://postgres:postgres@127.0.0.1:54322/postgres" \
  --schema wallet \
  --out crates/spike-typesafe-supabase/src/generated.rs
```

Output: `enums=17, composites=17, tables=15, fns=40` → 1898 lines of generated.rs in 0.3 s.

### `crates/spike-typesafe-supabase/`

Thin lib crate that just `pub mod generated;` and adds a `canary` module with literal struct constructions of `NewUsers` and `Args` for `upsert_user_with_accounts`. The canaries serve as drift sentinels — they're in `pub` (not `#[cfg(test)]`) so a missing field breaks `cargo build`, not just `cargo test`.

`compile_tests` module exercises the 4 acceptance criteria:

```
test compile_tests::enum_variants_match_sql_labels ... ok
test compile_tests::rpc_args_serialize_with_p_prefixed_param_names ... ok
test compile_tests::typed_column_names_exist ... ok
test compile_tests::typed_table_names_exist ... ok
```

## Acceptance criteria — proven against the live schema

| # | Criterion | Where it's proven |
|---|---|---|
| 1 | Type `users` (read + insert) with compile-time column checks | `canary::new_user_literal()` in `lib.rs`; `tables::users::{UsersRow, NewUsers, columns::*}` in `generated.rs` |
| 2 | Type `accounts` (with FK + enum like `state`) | `tables::accounts::{AccountsRow, NewAccounts}`; `enums::AccountState`, `enums::AccountType`, `enums::AccountPurpose` — all emitted with `serde(rename = "...")` matching SQL labels exactly (`active`, `cashu`, `gift-card`) |
| 3 | Type `upsert_user_with_accounts` (composite input + structured output) | `rpcs::upsert_user_with_accounts::{NAME, Args}`; `composites::AccountInput` — generated automatically from `pg_type` |
| 4 | New required column breaks the Rust build | `scripts/demo-drift.sh` — runs end-to-end against the live DB. Adds `drift_demo_required text not null` to `wallet.users`, regenerates, asserts `cargo build` fails with `missing field 'drift_demo_required' in initializer of NewUsers`, then reverts the column |

## Drift-detection story for the recommended path

In CI:

1. `bun run db:generate-types` already runs `supabase gen types typescript --local …` against the local stack. **Mirror it for Rust:**
   ```bash
   cargo run -q -p agicash-storage-supabase-codegen -- \
     --database-url "$LOCAL_SUPABASE_DB_URL" --schema wallet \
     --out crates/agicash-storage-supabase/src/generated.rs
   ```
2. `git diff --exit-code crates/agicash-storage-supabase/src/generated.rs` — fails the build if the developer didn't regenerate after editing migrations.
3. `cargo build -p agicash-storage-supabase --all-targets` — fails if a new required field/arg isn't reflected at any call site.
4. (Belt-and-suspenders) keep the existing integration tests against the real Supabase as the runtime smoke check.

This catches schema-vs-code drift before merge. Local devs do `just regen-storage` (one command) after touching `supabase/migrations/`.

## Gotchas surfaced by the PoC

These are real bugs you'll trip over if you productionize Path C without fixing them first.

1. **Function-arg nullability is invisible to introspection.** `pg_get_function_arguments(oid)` tells us the type and the `DEFAULT` clause but NOT whether `NULL` is a legal value. A `text` param IS nullable in Postgres but our codegen treats it as `String` unless it has `DEFAULT NULL`. The current `upsert_user_with_accounts` has `p_email text` (no default) — we emit `p_email: String`, but the SQL accepts `NULL`. Fix: convention. Either (a) every nullable param in our schema MUST be `DEFAULT NULL` so introspection sees it, or (b) annotate via a side-channel like a comment (`COMMENT ON FUNCTION … IS 'nullable: p_email,p_terms_accepted_at'`). Recommend (a) — it's a 1-line migration per affected fn and self-documenting.

2. **`_result` composite return types are dynamic.** `wallet.upsert_user_with_accounts_result` is `(user wallet.users, accounts jsonb[])`. PostgREST encodes this as `{ "user": {...}, "accounts": [{...},{...}] }`. The codegen currently emits `pub type Returns = serde_json::Value;` for these. Fix: easy follow-up — synthesize a concrete struct per `*_result` composite by recursively expanding row-typed fields (`wallet.users` → `tables::users::UsersRow`), and for `jsonb`/`jsonb[]` look at the function body to find a `to_jsonb(<row>)` to recover the shape. The lazy version: keep `serde_json::Value` for `_result` composites with `jsonb` fields, full struct for the rest. Today's `agicash-traits` already hand-models these (`UpsertUserResult`) — productionization can keep the hand model for the `jsonb` arms and just add `#[cfg(test)] assert_payload_shape!()` to verify the JSON keys against the composite.

3. **`column_default` is set even for `gen_random_uuid()` triggers.** `wallet.users.id` shows `column_default = gen_random_uuid()`, so `NewUsers.id` is `Option<Uuid>` — good. But `wallet.users.username` is filled by a `BEFORE INSERT` trigger (`set_default_username`) — `column_default` is NULL, so codegen marks `username` as required even though callers don't supply it. Fix: convention — for any column auto-set by a trigger, set a placeholder `DEFAULT` (e.g. `DEFAULT ''`) so codegen sees it as optional. Or: maintain a small allowlist in the codegen tool.

4. **`security definer` vs `security invoker` doesn't affect codegen** but matters for RLS impersonation. Path C is unaffected — postgrest's JWT auth path is unchanged.

5. **`postgrest::Builder::eq(col, val)` still takes `&str` for the column name.** Compile-time check works via `tables::users::columns::ID` being a `pub const &str` — typos against generated column names are catch-able if we lint for "any literal string passed to `.eq()` must come from a column constant". A clippy lint or a custom wrapper `Builder` type can enforce this. Out of scope for the spike; recommend wrapping postgrest's `Builder` with a `TypedBuilder<T>` that only accepts column constants from `T::columns`.

6. **`jsonb` columns are `serde_json::Value`** by default. For things like `accounts.details` and `transactions.metadata` this is intentional — schema doesn't type them. But several `jsonb` columns in agicash DO have a canonical shape (cashu proof witness, etc.). Path C punts on this; production can add per-column "json schema annotations" via comments and have codegen import a hand-written `pub type AccountDetails = ...` for those columns.

7. **The `TABLE(username text, id uuid)` return type warning** — `find_contact_candidates` returns a setof-row, which `pg_get_function_result` formats as `TABLE(...)`. We emit `serde_json::Value`. The codegen tool already warns about this. Fix: parse the `TABLE(...)` syntax and synthesize an anon struct.

8. **No automatic FK-typed Ids.** Today `agicash-domain` has `UserId(Uuid)`, `AccountId(Uuid)` newtypes — generated code uses plain `uuid::Uuid`. Path C should hook a mapping table (`{"wallet.users.id": "UserId", "wallet.accounts.user_id": "UserId"}`) so productionization keeps the existing newtypes.

## Why NOT sqlx (path A)

Worth being explicit because sqlx is the default Rust answer for "compile-time typed SQL". Constraint #1 (stay with Supabase) and the agicash architecture (PostgREST + RLS via JWT) rule it out. To use sqlx you'd:

- Replace the `postgrest` crate with `sqlx::PgPool`
- Wire JWT impersonation manually per request (`SET LOCAL request.jwt.claims = ...`, `SET LOCAL ROLE authenticated`)
- Rewrite every RPC call as raw SQL (`select * from wallet.upsert_user_with_accounts($1, $2, …)`)
- Lose Supabase's built-in PostgREST filter/embed syntax — anything fancy becomes hand-rolled SQL

You'd get great query-level type safety, but at the cost of:
- A second RLS path to audit (your impersonation code) on top of PostgREST's (which is already battle-tested)
- ~100% rewrite of the storage layer
- Divergence from the TypeScript path (which uses PostgREST via `@supabase/supabase-js`) — both sides should agree on how a wire request is shaped

Not worth it for a wallet whose threat model relies on RLS being right.

## Recommendation

**Path C — custom codegen.** Spec it for productionization as:

1. Move `spike-typesafe-supabase-codegen` to `crates/agicash-storage-supabase-codegen` (or a `xtask/codegen/` if the workspace prefers).
2. Wire the codegen into `just regen` (or similar) and CI gate on `git diff --exit-code`.
3. Fix the 8 gotchas above (most are 1–4 hour fixes; #2 and #8 are the longest).
4. Replace hand-written types in `agicash-traits::user_storage` and `agicash-domain::{user, account}` with the generated ones, keeping a small "domain adapter" layer to preserve newtype Ids and any business invariants.
5. Migrate `agicash-storage-supabase` call sites one by one (15 tables × a handful of accesses each — incremental, low-risk).

**Fallback:** Path D (name constants only). 1-day job. Catches typos in table/column/RPC names but nothing else. Worth doing as a safety net even if Path C gets prioritized later.

## Effort estimate to productionize Path C

**Size: M** (1.5–2.5 weeks of focused work, parallelizable across 2 workers if needed)

Lane breakdown (each is a slice-sized PR):

| Lane | Scope | Effort |
|---|---|---|
| **C1: Codegen tool hardening** | Fix gotchas #1, #3, #7. Add newtype-Id mapping (#8). Add JSON-schema annotation support (#6). Synthesize concrete struct for `_result` composites (#2). | 3–4 days |
| **C2: CI integration** | `just regen-storage` recipe. CI step: run codegen against local Supabase + `git diff --exit-code`. Update `db:generate-types` docs. | 1 day |
| **C3: Typed-builder wrapper** | `TypedBuilder<T: Table>` around `postgrest::Builder` that only accepts column constants from `T::columns`. Prevents the `.eq("misspelled_col", ...)` foot-gun (gotcha #5). | 2 days |
| **C4: Migrate `user_storage.rs`** | First real migration. Replace `UpsertUserInput`, `UpsertUserResult` with generated types + thin adapters that preserve `UserId`/`AccountId` newtypes. All existing tests must still pass. | 2 days |
| **C5: Migrate remaining storage modules** | `cashu_mint_quote_storage.rs`, `cashu_melt_quote_storage.rs`, `cashu_receive_swap_storage.rs`, `cashu_send_swap_storage.rs`. Same pattern. | 3–4 days |
| **C6: Documentation + sunset old types** | Mark hand-written `UpsertUserInput` etc. as `#[deprecated]` or delete. Add a "schema change workflow" doc. | 1 day |

Pure-codegen lanes (C1+C2+C3) are independent of migration lanes (C4–C5) and can be done first to derisk.

## Open questions for gudnuf

1. **Newtype Ids.** Today `agicash-domain` has `UserId(Uuid)`, `AccountId(Uuid)`. Generated code defaults to `uuid::Uuid`. Should the codegen emit `UserId` everywhere a column has FK to `wallet.users.id`, etc.? Or keep newtypes only at the domain boundary and use bare `Uuid` in the storage layer? (My pref: codegen with newtypes, since the migration to typed columns is mostly mechanical.)
2. **Where does the codegen DB come from in CI?** The PoC assumes a running local Supabase (which we have for dev). For CI, do we want (a) `supabase start` in the CI workflow (slow, ~30 s startup), (b) raw `psql` against a `postgres:17` container with the migration files applied (faster, ~5 s), or (c) `testcontainers-rs` from inside the codegen tool itself (most hermetic but adds runtime deps)? My pref: (b) — fastest and the migrations are the source of truth either way.
3. **Convention enforcement.** Gotcha #1 (`DEFAULT NULL` for nullable RPC args) and gotcha #3 (placeholder defaults for trigger-set columns) are conventions, not magic. Are you OK adopting them, or do you want the codegen to support side-channel annotations (comments) for these cases? Conventions are simpler; annotations are friendlier to existing migrations.
4. **`jsonb`-schema annotations.** Do we want a per-column type override (e.g. `COMMENT ON COLUMN wallet.accounts.details IS '@rust-type: agicash_domain::AccountDetails'`)? Or live with `serde_json::Value` and validate in adapters? Depends on how much we care about typed `jsonb`.
5. **Tabula symmetry.** TypeScript already has `database.types.ts`. Should the Rust codegen aim to be schema-equivalent (so the two sides agree on field names/optionality byte-for-byte), or is "introspected from the same migrations" good enough?

## Files in this spike

- `crates/spike-typesafe-supabase/codegen/Cargo.toml`
- `crates/spike-typesafe-supabase/codegen/src/main.rs` — the codegen tool (~430 LOC)
- `crates/spike-typesafe-supabase/Cargo.toml`
- `crates/spike-typesafe-supabase/src/lib.rs` — canaries + compile_tests (~140 LOC)
- `crates/spike-typesafe-supabase/src/generated.rs` — emitted output (~1900 LOC, regenerable)
- `crates/spike-typesafe-supabase/scripts/demo-drift.sh` — end-to-end drift demo
- `docs/superpowers/specs/2026-05-16-typesafe-supabase-spike.md` — this report

## Reproduce

```bash
cd ~/agicash/.claude/worktrees/typesafe-storage-spike
export PATH=$HOME/.cargo/bin:$PATH

# 1. Codegen against the local Supabase that's already running.
cd crates
cargo run -p spike-typesafe-supabase-codegen -- \
  --database-url "postgres://postgres:postgres@127.0.0.1:54322/postgres" \
  --schema wallet \
  --out spike-typesafe-supabase/src/generated.rs

# 2. Run compile_tests (criteria 1–3).
cargo test -p spike-typesafe-supabase

# 3. Run the drift demo (criterion 4).
cd ..
bash crates/spike-typesafe-supabase/scripts/demo-drift.sh
```
