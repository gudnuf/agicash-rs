-- Add a partial unique index on cashu_send_quotes(user_id, payment_hash)
-- covering UNPAID, PENDING and PAID quotes. The only pre-existing uniqueness
-- guard on this table keys on quote_id_hash (the SHA-256 of the *mint-side*
-- melt-quote id), which the mint re-issues fresh on every POST
-- /v1/melt/quote/bolt11 -- so it never collides for a re-quote of the same
-- invoice and gives zero protection against the paste-invoice / re-quote
-- double-pay class. The stable identity of "this is the same payment" is the
-- bolt11 invoice's payment_hash (already persisted in plaintext on the row).
--
-- Including PAID closes the dangerous case (a fresh quote for an
-- already-paid invoice firing a second real post_melt); UNPAID and PENDING
-- close the concurrent / in-flight double-tap (race-safe: holds even under
-- two concurrent create_cashu_send_quote calls for the same invoice, which
-- the CONCURRENCY_ERROR proof-reservation guard does NOT cover because the
-- two quotes select different UNSPENT proofs and both otherwise succeed).
-- FAILED and EXPIRED are intentionally excluded so a genuinely failed /
-- expired attempt can be legitimately retried.
--
-- This mirrors the existing spark-side precedent migration
-- 20260425181643_tighten_spark_send_payment_hash_uniqueness.sql
-- (cashu_send_quote_state's PAID is the analog of spark_send_quote_state's
-- COMPLETED). Additive only: a pure CREATE UNIQUE INDEX, safe to apply
-- forward; the drop-if-exists keeps the migration re-runnable.

drop index if exists "wallet"."cashu_send_quotes_payment_hash_active_unique";

create unique index "cashu_send_quotes_payment_hash_active_unique"
  on "wallet"."cashu_send_quotes" ("user_id", "payment_hash")
  where ("state" = any (array['UNPAID', 'PENDING', 'PAID']::"wallet"."cashu_send_quote_state"[]));
