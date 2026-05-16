//! Spike: typed Supabase access via codegen.
//!
//! Approach: a sibling crate (`spike-typesafe-supabase-codegen`) connects to a
//! Postgres database with `supabase/migrations/*.sql` already applied,
//! introspects the `wallet` schema, and writes `src/generated.rs`. Application
//! code then uses `tables::*` and `rpcs::*` modules — each one is a thin typed
//! wrapper over the existing `postgrest::Postgrest` client. Wire format stays
//! PostgREST (so RLS still applies); only the spelling of table/column names
//! and the shape of insert/return payloads is enforced at compile time.
//!
//! See `docs/superpowers/specs/2026-05-16-typesafe-supabase-spike.md` for the
//! spike report and `examples/poc.rs` for a worked demo against the local
//! Supabase stack.

pub mod generated;

pub use generated::{composites, enums, rpcs, tables};

/// Canary call sites. These functions are never called at runtime — they
/// exist so that a schema migration adding a new required column or arg breaks
/// the build of *this crate*, not just its tests. The drift-detection demo
/// (`scripts/demo-drift.sh`) relies on this.
///
/// In a real productionization, every call site in `agicash-storage-supabase`
/// becomes a canary for its own shape; this module is a stand-in for that.
pub mod canary {
    use super::{composites, enums, rpcs, tables};

    /// Insert into `wallet.users`. Adding a NOT NULL column without a default
    /// to wallet.users breaks this function.
    #[allow(dead_code)]
    pub fn new_user_literal() -> tables::users::NewUsers {
        tables::users::NewUsers {
            id: None,
            created_at: None,
            email: Some("u@example.com".into()),
            email_verified: true,
            updated_at: None,
            default_btc_account_id: Some(uuid::Uuid::nil()),
            default_currency: None,
            default_usd_account_id: None,
            username: "user-x".into(),
            cashu_locking_xpub: "x".into(),
            encryption_public_key: "e".into(),
            spark_identity_public_key: "s".into(),
            terms_accepted_at: None,
            gift_card_mint_terms_accepted_at: None,
        }
    }

    /// Call `wallet.upsert_user_with_accounts`. Adding a required arg to the
    /// SQL function breaks this function.
    #[allow(dead_code)]
    pub fn upsert_args_literal() -> rpcs::upsert_user_with_accounts::Args {
        rpcs::upsert_user_with_accounts::Args {
            p_user_id: uuid::Uuid::nil(),
            p_email: "u@example.com".into(),
            p_email_verified: true,
            p_accounts: vec![composites::AccountInput {
                r#type: enums::AccountType::Spark,
                purpose: enums::AccountPurpose::Transactional,
                currency: enums::Currency::Btc,
                name: "Lightning".into(),
                details: serde_json::json!({ "network": "MAINNET" }),
                is_default: true,
            }],
            p_cashu_locking_xpub: "x".into(),
            p_encryption_public_key: "e".into(),
            p_spark_identity_public_key: "s".into(),
            p_terms_accepted_at: None,
            p_gift_card_mint_terms_accepted_at: None,
        }
    }
}

/// Re-export so callers don't have to depend on postgrest directly when all
/// they want is the typed wrappers.
pub use postgrest::{Builder, Postgrest};

#[cfg(test)]
mod compile_tests {
    //! These tests don't run code — they verify at compile time that the
    //! generated bindings match what the production storage layer needs.
    //! Adding a new required column or removing one will break these.

    use super::*;

    #[test]
    fn typed_table_names_exist() {
        assert_eq!(tables::users::NAME, "users");
        assert_eq!(tables::accounts::NAME, "accounts");
    }

    #[test]
    fn typed_column_names_exist() {
        assert_eq!(tables::users::columns::ID, "id");
        assert_eq!(tables::users::columns::EMAIL, "email");
        assert_eq!(tables::accounts::columns::STATE, "state");
    }

    #[test]
    fn enum_variants_match_sql_labels() {
        let s = serde_json::to_string(&enums::AccountState::Active).unwrap();
        assert_eq!(s, "\"active\"");
        let s = serde_json::to_string(&enums::AccountType::Cashu).unwrap();
        assert_eq!(s, "\"cashu\"");
        let s = serde_json::to_string(&enums::Currency::Btc).unwrap();
        assert_eq!(s, "\"BTC\"");
    }

    #[test]
    fn rpc_args_serialize_with_p_prefixed_param_names() {
        let args = rpcs::upsert_user_with_accounts::Args {
            p_user_id: uuid::Uuid::nil(),
            // NB: Postgres `text` arg is nullable but pg_get_function_arguments
            // can't tell us that. Codegen treats every IN arg as non-nullable
            // unless it has DEFAULT. See `Gotcha: function-arg nullability`
            // in the spike report.
            p_email: "u@example.com".into(),
            p_email_verified: true,
            p_accounts: vec![composites::AccountInput {
                r#type: enums::AccountType::Spark,
                purpose: enums::AccountPurpose::Transactional,
                currency: enums::Currency::Btc,
                name: "Lightning".into(),
                details: serde_json::json!({ "network": "MAINNET" }),
                is_default: true,
            }],
            p_cashu_locking_xpub: "xpub".into(),
            p_encryption_public_key: "enc".into(),
            p_spark_identity_public_key: "spark".into(),
            p_terms_accepted_at: None,
            p_gift_card_mint_terms_accepted_at: None,
        };
        let v = serde_json::to_value(&args).unwrap();
        assert!(v.get("p_user_id").is_some());
        assert!(v.get("p_accounts").is_some());
        // Optional with-default param is omitted when None.
        assert!(v.get("p_terms_accepted_at").is_none());
    }

    /// The whole point of the spike. Demonstrates one call site that touches
    /// table name + column name + insert shape + RPC name + RPC args — every
    /// component checked against the generated module.
    #[allow(dead_code)]
    fn one_call_site_per_concern(client: &Postgrest) {
        // Table + column name compile-checked.
        let _ = tables::users::from(client)
            .select("*")
            .eq(tables::users::columns::ID, "x");

        // Insert payload compile-checked (defaulted fields are Option, NOT-NULL
        // ones are required). Note `username` is NOT NULL with no default at the
        // schema level (the trigger fills it in) — see "drift gotchas" in the
        // spike report.
        let _new = tables::users::NewUsers {
            id: None,
            created_at: None,
            email: Some("u@example.com".into()),
            email_verified: true,
            updated_at: None,
            default_btc_account_id: Some(uuid::Uuid::nil()),
            default_currency: None,
            default_usd_account_id: None,
            username: "user-x".into(),
            cashu_locking_xpub: "x".into(),
            encryption_public_key: "e".into(),
            spark_identity_public_key: "s".into(),
            terms_accepted_at: None,
            gift_card_mint_terms_accepted_at: None,
        };

        // RPC compile-checked.
        let _ = client.rpc(
            rpcs::upsert_user_with_accounts::NAME,
            serde_json::to_string(&rpcs::upsert_user_with_accounts::Args {
                p_user_id: uuid::Uuid::nil(),
                p_email: String::new(),
                p_email_verified: false,
                p_accounts: vec![],
                p_cashu_locking_xpub: "x".into(),
                p_encryption_public_key: "e".into(),
                p_spark_identity_public_key: "s".into(),
                p_terms_accepted_at: None,
                p_gift_card_mint_terms_accepted_at: None,
            })
            .unwrap(),
        );
    }
}
