//! Codegen for typed Supabase access.
//!
//! Connects to a Postgres database that has the migrations already applied
//! (e.g., `supabase start` against `supabase/migrations/*.sql`) and emits a
//! `generated.rs` containing:
//!
//! - One `pub mod tables::<table>` per base table in the target schema,
//!   exposing `Row` (deser), `Insert` (ser), strongly-typed column names,
//!   and a `from()` helper that wraps `postgrest::Postgrest::from(...)`.
//! - One `pub mod rpcs::<fn>` per function in the target schema, exposing
//!   `Args` (ser) and `Returns` (deser) types and a typed call wrapper.
//! - One `pub enum` per Postgres `enum` type, with `serde(rename = "...")`
//!   matching the SQL label exactly.
//!
//! NB: this is a spike PoC. Sufficient to validate the approach against the
//! `users`, `accounts`, and `upsert_user_with_accounts` slice. JSONB columns
//! deserialize as `serde_json::Value`. Composite types used as arguments
//! flow through as nested structs that postgrest sends as JSON; PostgREST
//! accepts JSON objects for composite parameters since v9 — see
//! tests in the spike report.

use anyhow::{anyhow, Result};
use clap::Parser;
use heck::{ToPascalCase, ToSnakeCase};
use indoc::writedoc;
use postgres::{Client, NoTls};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "Spike: typed-Supabase codegen", long_about = None)]
struct Cli {
    /// Postgres URL — typically the local supabase db (port 54322).
    #[arg(long)]
    database_url: String,
    /// Schema to scan (we use `wallet`).
    #[arg(long, default_value = "wallet")]
    schema: String,
    /// Output path for the generated file.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, Clone)]
struct EnumDef {
    name: String,
    variants: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompositeField {
    name: String,
    pg_type: String, // raw pg_catalog.format_type output, e.g. `wallet.currency`, `text`, `jsonb[]`
}

#[derive(Debug, Clone)]
struct CompositeDef {
    name: String,
    fields: Vec<CompositeField>,
}

#[derive(Debug, Clone)]
struct ColumnDef {
    name: String,
    data_type: String,    // pg_catalog.format_type
    udt_schema: String,   // e.g. "wallet" for enums/composites, "pg_catalog" for builtins
    udt_name: String,     // e.g. "currency", "text", "uuid"
    is_nullable: bool,
    has_default: bool,
}

#[derive(Debug, Clone)]
struct TableDef {
    name: String,
    columns: Vec<ColumnDef>,
}

#[derive(Debug, Clone)]
struct FnArg {
    name: String,
    pg_type: String, // e.g. `wallet.account_input[]`
    has_default: bool,
}

#[derive(Debug, Clone)]
struct FnDef {
    name: String,
    args: Vec<FnArg>,
    return_type: String, // pg type expression
    return_is_set: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut client = Client::connect(&cli.database_url, NoTls)?;

    let enums = load_enums(&mut client, &cli.schema)?;
    let composites = load_composites(&mut client, &cli.schema)?;
    let tables = load_tables(&mut client, &cli.schema)?;
    let fns = load_functions(&mut client, &cli.schema)?;

    let mut out = String::new();
    write_header(&mut out, &cli.schema);
    write_enums(&mut out, &enums)?;
    write_composites(&mut out, &composites, &enums)?;
    write_tables(&mut out, &tables, &enums, &composites)?;
    write_rpcs(&mut out, &fns, &enums, &composites)?;

    if let Some(parent) = cli.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&cli.out, out)?;
    eprintln!(
        "wrote {} (enums={}, composites={}, tables={}, fns={})",
        cli.out.display(),
        enums.len(),
        composites.len(),
        tables.len(),
        fns.len()
    );
    Ok(())
}

fn load_enums(client: &mut Client, schema: &str) -> Result<BTreeMap<String, EnumDef>> {
    let rows = client.query(
        "select t.typname,
                array(select enumlabel from pg_enum e where e.enumtypid = t.oid order by e.enumsortorder)
         from pg_type t
         join pg_namespace n on n.oid = t.typnamespace
         where n.nspname = $1 and t.typtype = 'e'
         order by t.typname",
        &[&schema],
    )?;
    let mut out = BTreeMap::new();
    for row in rows {
        let name: String = row.get(0);
        let variants: Vec<String> = row.get(1);
        out.insert(name.clone(), EnumDef { name, variants });
    }
    Ok(out)
}

fn load_composites(client: &mut Client, schema: &str) -> Result<BTreeMap<String, CompositeDef>> {
    let rows = client.query(
        "select t.typname,
                a.attname,
                pg_catalog.format_type(a.atttypid, a.atttypmod)
         from pg_type t
         join pg_namespace n on n.oid = t.typnamespace
         join pg_class c on c.oid = t.typrelid
         join pg_attribute a on a.attrelid = c.oid
         where n.nspname = $1
           and t.typtype = 'c'
           and c.relkind = 'c'   -- standalone composite type only (excludes tables)
           and a.attnum > 0
           and not a.attisdropped
         order by t.typname, a.attnum",
        &[&schema],
    )?;
    let mut by_name: BTreeMap<String, CompositeDef> = BTreeMap::new();
    for row in rows {
        let typname: String = row.get(0);
        let attname: String = row.get(1);
        let typ: String = row.get(2);
        by_name
            .entry(typname.clone())
            .or_insert(CompositeDef {
                name: typname,
                fields: Vec::new(),
            })
            .fields
            .push(CompositeField {
                name: attname,
                pg_type: typ,
            });
    }
    Ok(by_name)
}

fn load_tables(client: &mut Client, schema: &str) -> Result<Vec<TableDef>> {
    let table_rows = client.query(
        "select table_name from information_schema.tables
         where table_schema = $1 and table_type = 'BASE TABLE'
         order by table_name",
        &[&schema],
    )?;
    let mut out = Vec::new();
    for trow in table_rows {
        let table_name: String = trow.get(0);
        let col_rows = client.query(
            "select column_name, data_type, udt_schema, udt_name, is_nullable, column_default
             from information_schema.columns
             where table_schema = $1 and table_name = $2
             order by ordinal_position",
            &[&schema, &table_name],
        )?;
        let columns = col_rows
            .into_iter()
            .map(|r| {
                let name: String = r.get(0);
                let data_type: String = r.get(1);
                let udt_schema: String = r.get(2);
                let udt_name: String = r.get(3);
                let nullable: String = r.get(4);
                let default: Option<String> = r.get(5);
                ColumnDef {
                    name,
                    data_type,
                    udt_schema,
                    udt_name,
                    is_nullable: nullable == "YES",
                    has_default: default.is_some(),
                }
            })
            .collect();
        out.push(TableDef {
            name: table_name,
            columns,
        });
    }
    Ok(out)
}

fn load_functions(client: &mut Client, schema: &str) -> Result<Vec<FnDef>> {
    // Skip trigger functions (returning trigger) and the broadcast helpers — they're
    // not user-callable RPCs from the REST layer.
    let rows = client.query(
        "select p.proname,
                pg_get_function_arguments(p.oid),
                pg_get_function_result(p.oid),
                p.proretset
         from pg_proc p
         join pg_namespace n on n.oid = p.pronamespace
         where n.nspname = $1
           and p.prokind = 'f'
           and pg_get_function_result(p.oid) not in ('trigger', 'event_trigger')
         order by p.proname",
        &[&schema],
    )?;
    let mut out = Vec::new();
    for row in rows {
        let name: String = row.get(0);
        let args_str: String = row.get(1);
        let result_type: String = row.get(2);
        let retset: bool = row.get(3);
        let args = parse_fn_args(&args_str)?;
        out.push(FnDef {
            name,
            args,
            return_type: result_type,
            return_is_set: retset,
        });
    }
    Ok(out)
}

/// Parse `pg_get_function_arguments` output, e.g.
///   `p_user_id uuid, p_email text, p_email_verified boolean,
///    p_accounts wallet.account_input[],
///    p_terms_accepted_at timestamp with time zone DEFAULT NULL::timestamp with time zone`
///
/// We need to split on commas that aren't inside parens/brackets (e.g. numeric(10,2)).
fn parse_fn_args(s: &str) -> Result<Vec<FnArg>> {
    if s.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parts = split_top_level_commas(s);
    let mut out = Vec::new();
    for part in parts {
        let part = part.trim();
        // Format: `[mode] name type [DEFAULT expr]`. Modes (IN/OUT/INOUT) we
        // ignore for now — Supabase functions are all IN-only.
        let lower = part.to_lowercase();
        let (lhs, has_default) = if let Some(idx) = lower.find(" default ") {
            (&part[..idx], true)
        } else {
            (part, false)
        };
        let mut sp = lhs.splitn(2, char::is_whitespace);
        let name = sp
            .next()
            .ok_or_else(|| anyhow!("empty arg: {part}"))?
            .to_string();
        let pg_type = sp
            .next()
            .ok_or_else(|| anyhow!("missing type for arg `{name}`: {part}"))?
            .trim()
            .to_string();
        out.push(FnArg {
            name,
            pg_type,
            has_default,
        });
    }
    Ok(out)
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn write_header(out: &mut String, schema: &str) {
    writedoc!(
        out,
        r#"
            // @generated by spike-typesafe-supabase-codegen — DO NOT EDIT.
            // Source: introspection of schema `{schema}` from a Postgres DB
            // with `supabase/migrations/*.sql` applied.
            //
            // Regenerate with:
            //   cargo run -p spike-typesafe-supabase-codegen -- \
            //     --database-url "$DATABASE_URL" --schema {schema} \
            //     --out crates/spike-typesafe-supabase/src/generated.rs
            #![allow(dead_code, clippy::needless_pub_self, clippy::module_name_repetitions)]
            use serde::{{Deserialize, Serialize}};

        "#,
        schema = schema
    )
    .unwrap();
}

fn write_enums(out: &mut String, enums: &BTreeMap<String, EnumDef>) -> Result<()> {
    writedoc!(out, "pub mod enums {{\n    use super::*;\n\n").unwrap();
    for e in enums.values() {
        let rust_name = e.name.to_pascal_case();
        writeln!(out, "    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]").unwrap();
        writeln!(out, "    pub enum {} {{", rust_name).unwrap();
        for v in &e.variants {
            let variant = sql_label_to_variant(v);
            writeln!(out, "        #[serde(rename = \"{v}\")]").unwrap();
            writeln!(out, "        {variant},").unwrap();
        }
        writeln!(out, "    }}\n").unwrap();
    }
    writedoc!(out, "}}\n\n").unwrap();
    Ok(())
}

fn write_composites(
    out: &mut String,
    composites: &BTreeMap<String, CompositeDef>,
    enums: &BTreeMap<String, EnumDef>,
) -> Result<()> {
    writedoc!(out, "pub mod composites {{\n    use super::*;\n\n").unwrap();
    for c in composites.values() {
        // Skip the *_result composites; we represent those inline per RPC since
        // postgrest returns them as the function body (not via REST encoding of
        // the composite). They're regenerated as Returns next to the RPC.
        if c.name.ends_with("_result") {
            continue;
        }
        let rust_name = c.name.to_pascal_case();
        writeln!(out, "    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]").unwrap();
        writeln!(out, "    pub struct {} {{", rust_name).unwrap();
        for f in &c.fields {
            let rust_field = sanitize_ident(&f.name.to_snake_case());
            let rust_ty = pg_to_rust_type(&f.pg_type, enums, composites, false);
            if f.name != rust_field {
                writeln!(out, "        #[serde(rename = \"{}\")]", f.name).unwrap();
            }
            writeln!(out, "        pub {rust_field}: {rust_ty},").unwrap();
        }
        writeln!(out, "    }}\n").unwrap();
    }
    writedoc!(out, "}}\n\n").unwrap();
    Ok(())
}

fn write_tables(
    out: &mut String,
    tables: &[TableDef],
    enums: &BTreeMap<String, EnumDef>,
    composites: &BTreeMap<String, CompositeDef>,
) -> Result<()> {
    writedoc!(out, "pub mod tables {{\n    use super::*;\n\n").unwrap();
    for t in tables {
        let mod_name = sanitize_ident(&t.name.to_snake_case());
        let row_name = format!("{}Row", t.name.to_pascal_case());
        let insert_name = format!("New{}", t.name.to_pascal_case());

        writeln!(out, "    pub mod {mod_name} {{").unwrap();
        writeln!(out, "        use super::*;").unwrap();
        writeln!(out, "        /// PostgREST table identifier — checked against migrations at codegen time.").unwrap();
        writeln!(out, "        pub const NAME: &str = \"{}\";\n", t.name).unwrap();

        // Column-name constants — compile-time guarantee that .eq(column::ID, ...) wraps the right name.
        writeln!(out, "        pub mod columns {{").unwrap();
        for c in &t.columns {
            let cname = c.name.to_uppercase();
            writeln!(out, "            pub const {cname}: &str = \"{}\";", c.name).unwrap();
        }
        writeln!(out, "        }}\n").unwrap();

        // Row struct — what SELECT * returns. Always uses serde rename when needed.
        writeln!(out, "        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]").unwrap();
        writeln!(out, "        pub struct {row_name} {{").unwrap();
        for c in &t.columns {
            let rust_field = sanitize_ident(&c.name.to_snake_case());
            let base_ty = column_to_rust_type(c, enums, composites);
            let ty = if c.is_nullable {
                format!("Option<{base_ty}>")
            } else {
                base_ty
            };
            if c.name != rust_field {
                writeln!(out, "            #[serde(rename = \"{}\")]", c.name).unwrap();
            }
            writeln!(out, "            pub {rust_field}: {ty},").unwrap();
        }
        writeln!(out, "        }}\n").unwrap();

        // Insert struct — columns without defaults are required; nullable-with-default ones are optional.
        // No Default derive: required enum columns wouldn't satisfy it.
        writeln!(out, "        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]").unwrap();
        writeln!(out, "        pub struct {insert_name} {{").unwrap();
        for c in &t.columns {
            let rust_field = sanitize_ident(&c.name.to_snake_case());
            let base_ty = column_to_rust_type(c, enums, composites);
            let required = !c.is_nullable && !c.has_default;
            let ty = if required {
                base_ty
            } else {
                format!("Option<{base_ty}>")
            };
            if c.name != rust_field {
                writeln!(out, "            #[serde(rename = \"{}\")]", c.name).unwrap();
            }
            if !required {
                writeln!(out, "            #[serde(skip_serializing_if = \"Option::is_none\")]").unwrap();
            }
            writeln!(out, "            pub {rust_field}: {ty},").unwrap();
        }
        writeln!(out, "        }}\n").unwrap();

        // Thin helper — keeps caller from spelling the table name.
        writeln!(out, "        /// Returns a postgrest builder pre-bound to this table.").unwrap();
        writeln!(out, "        pub fn from(client: &postgrest::Postgrest) -> postgrest::Builder {{").unwrap();
        writeln!(out, "            client.from(NAME)").unwrap();
        writeln!(out, "        }}").unwrap();
        writeln!(out, "    }}\n").unwrap();
    }
    writedoc!(out, "}}\n\n").unwrap();
    Ok(())
}

fn write_rpcs(
    out: &mut String,
    fns: &[FnDef],
    enums: &BTreeMap<String, EnumDef>,
    composites: &BTreeMap<String, CompositeDef>,
) -> Result<()> {
    writedoc!(out, "pub mod rpcs {{\n    use super::*;\n\n").unwrap();
    for f in fns {
        let mod_name = sanitize_ident(&f.name.to_snake_case());
        writeln!(out, "    pub mod {mod_name} {{").unwrap();
        writeln!(out, "        use super::*;").unwrap();
        writeln!(out, "        pub const NAME: &str = \"{}\";\n", f.name).unwrap();

        // Args struct — fields use the SQL parameter names verbatim
        // (postgrest sends the JSON object as the function body).
        writeln!(out, "        #[derive(Debug, Clone, Serialize)]").unwrap();
        writeln!(out, "        pub struct Args {{").unwrap();
        for a in &f.args {
            let rust_field = sanitize_ident(&a.name.to_snake_case());
            let base_ty = pg_to_rust_type(&a.pg_type, enums, composites, true);
            let ty = if a.has_default {
                format!("Option<{base_ty}>")
            } else {
                base_ty
            };
            if a.name != rust_field {
                writeln!(out, "            #[serde(rename = \"{}\")]", a.name).unwrap();
            }
            if a.has_default {
                writeln!(out, "            #[serde(skip_serializing_if = \"Option::is_none\")]").unwrap();
            }
            writeln!(out, "            pub {rust_field}: {ty},").unwrap();
        }
        writeln!(out, "        }}\n").unwrap();

        // Returns struct (or `Vec<Returns>` if proretset).
        // For composite return types (e.g. wallet.upsert_user_with_accounts_result),
        // generate the struct here based on the resolved composite fields.
        let returns_ty = render_rpc_return_type(f, enums, composites)?;
        writeln!(out, "        pub type Returns = {};\n", returns_ty).unwrap();

        writeln!(out, "    }}\n").unwrap();
    }
    writedoc!(out, "}}\n\n").unwrap();
    Ok(())
}

/// Render the rust return type for an RPC. If the return type is a composite
/// from our schema, we emit a struct literal inline (anon struct via a tuple-
/// like alias would be cleaner; for the spike we use serde_json::Value when
/// the shape is too dynamic). For simple scalars + setof, we map directly.
fn render_rpc_return_type(
    f: &FnDef,
    enums: &BTreeMap<String, EnumDef>,
    composites: &BTreeMap<String, CompositeDef>,
) -> Result<String> {
    let inner = pg_to_rust_type(&f.return_type, enums, composites, false);
    if f.return_is_set {
        Ok(format!("Vec<{inner}>"))
    } else {
        Ok(inner)
    }
}

/// Map a Postgres type expression to a Rust type. `for_serialize` flips the
/// preference for composite handling (incoming JSON body vs returned row).
fn pg_to_rust_type(
    pg: &str,
    enums: &BTreeMap<String, EnumDef>,
    composites: &BTreeMap<String, CompositeDef>,
    _for_serialize: bool,
) -> String {
    let trimmed = pg.trim();
    // Array form: `wallet.account_input[]` or `text[]`.
    if let Some(inner) = trimmed.strip_suffix("[]") {
        let inner_ty = pg_to_rust_type(inner.trim(), enums, composites, _for_serialize);
        return format!("Vec<{inner_ty}>");
    }
    // SETOF X — function return shape; handled at the FnDef.return_is_set
    // level. If we still see it here, strip it.
    if let Some(rest) = trimmed.strip_prefix("SETOF ").or_else(|| trimmed.strip_prefix("setof ")) {
        return pg_to_rust_type(rest, enums, composites, _for_serialize);
    }
    // Schema-qualified user type — look up in our enums/composites.
    if let Some((schema, name)) = trimmed.split_once('.') {
        if schema == "wallet" {
            if enums.contains_key(name) {
                return format!("super::super::enums::{}", name.to_pascal_case());
            }
            if let Some(c) = composites.get(name) {
                if c.name.ends_with("_result") {
                    // For RPC return composites we synthesize a serde_json::Value
                    // as a safe shape: postgrest serializes the composite as JSON
                    // with field names matching the SQL composite. A future
                    // iteration could emit a concrete struct here too.
                    return "serde_json::Value".to_string();
                }
                return format!("super::super::composites::{}", name.to_pascal_case());
            }
            // Schema-qualified row-type (e.g. wallet.users) — function returning
            // the table row. Map to the corresponding tables::*::Row.
            return format!(
                "super::super::tables::{}::{}Row",
                name.to_snake_case(),
                name.to_pascal_case()
            );
        }
    }
    // Built-in scalars.
    match trimmed {
        "uuid" => "uuid::Uuid".into(),
        "text" | "character varying" | "varchar" | "name" => "String".into(),
        "boolean" | "bool" => "bool".into(),
        "integer" | "int4" => "i32".into(),
        "smallint" | "int2" => "i16".into(),
        "bigint" | "int8" => "i64".into(),
        "real" | "float4" => "f32".into(),
        "double precision" | "float8" => "f64".into(),
        "timestamp with time zone" | "timestamptz" => {
            "chrono::DateTime<chrono::Utc>".into()
        }
        "timestamp without time zone" | "timestamp" => "chrono::NaiveDateTime".into(),
        "date" => "chrono::NaiveDate".into(),
        "jsonb" | "json" => "serde_json::Value".into(),
        "bytea" => "Vec<u8>".into(),
        // Numerics — keep as String to avoid pulling rust_decimal here.
        s if s.starts_with("numeric") => "String".into(),
        // Anything else: fall back to JSON Value so the spike compiles. A real
        // build should reject this and force the user to add a mapping.
        other => {
            eprintln!("WARN: no Rust mapping for pg type `{other}` — emitting serde_json::Value");
            "serde_json::Value".into()
        }
    }
}

fn column_to_rust_type(
    c: &ColumnDef,
    enums: &BTreeMap<String, EnumDef>,
    composites: &BTreeMap<String, CompositeDef>,
) -> String {
    // information_schema gives us udt_schema/udt_name which is cleaner than
    // re-parsing data_type for user-defined types.
    if c.udt_schema == "wallet" {
        if enums.contains_key(&c.udt_name) {
            return format!("super::super::enums::{}", c.udt_name.to_pascal_case());
        }
        if composites.contains_key(&c.udt_name) {
            return format!("super::super::composites::{}", c.udt_name.to_pascal_case());
        }
    }
    // Built-ins via the data_type column.
    pg_to_rust_type(&c.data_type, enums, composites, false)
}

/// Turn a SQL enum label into a Rust variant identifier.
fn sql_label_to_variant(s: &str) -> String {
    // PascalCase, replacing non-alphanumerics with underscores first.
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned.to_pascal_case()
}

fn sanitize_ident(s: &str) -> String {
    let reserved = [
        "type", "match", "ref", "mod", "fn", "use", "let", "self", "move", "trait", "impl",
        "loop", "for", "in", "while", "as", "if", "else", "return", "struct", "enum", "where",
    ];
    if reserved.contains(&s) {
        format!("r#{s}")
    } else {
        s.to_string()
    }
}
