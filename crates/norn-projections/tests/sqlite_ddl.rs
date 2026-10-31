#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v2 → SQLite DDL (T1-PROJ-2, #2529): the golden over the
//! object-type fixture corpus, determinism, and the fail-closed cases. That
//! the DDL applies on plain SQLite is proven in `norn-cell` (the only crate
//! that may link SQLite).

use std::path::{Path, PathBuf};

use norn_ir::{ObjectTypeDef, PropertyTypeDef};
use norn_projections::sqlite_ddl::{
    MAX_COLUMNS_PER_TABLE, OVERFLOW_COLUMN, SqliteDdlError, SqliteDdlInput, ValueEncoding,
    project_logical_sqlite_ddl, project_sqlite_ddl,
};

const UPDATE_ENV: &str = "NORN_PROJECTIONS_UPDATE_GOLDENS";
const GOLDEN: &str = "projections/sqlite/entity-v2-object-types.sql";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixture directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

/// Every object type of `fixtures/ir/entity-v2/object-types/`.
fn object_types() -> Vec<ObjectTypeDef> {
    json_files(&fixtures().join("ir/entity-v2/object-types"))
        .iter()
        .map(|path| {
            serde_json::from_str(&std::fs::read_to_string(path).expect("reads"))
                .unwrap_or_else(|error| panic!("{} decodes: {error}", path.display()))
        })
        .collect()
}

/// Every shared property type of `fixtures/ir/entity-v2/property-types/`,
/// plus the one registry value type they reference (`iso4217.currency_code`,
/// a Norn Core `text` type until the registry lands, T8-STD-1).
fn property_types() -> Vec<PropertyTypeDef> {
    let mut defs: Vec<PropertyTypeDef> =
        json_files(&fixtures().join("ir/entity-v2/property-types"))
            .iter()
            .map(|path| {
                serde_json::from_str(&std::fs::read_to_string(path).expect("reads"))
                    .unwrap_or_else(|error| panic!("{} decodes: {error}", path.display()))
            })
            .collect();
    defs.push(
        serde_json::from_value(serde_json::json!({
            "name": "iso4217.currency_code",
            "type": { "kind": "text" },
            "constraints": { "pattern": "^[A-Z]{3}$" }
        }))
        .expect("registry stand-in decodes"),
    );
    defs
}

/// The DDL of the fixture corpus is the checked-in golden
/// `fixtures/projections/sqlite/entity-v2-object-types.sql`. Regenerate with
/// `NORN_PROJECTIONS_UPDATE_GOLDENS=1`.
#[test]
fn fixture_corpus_ddl_matches_its_golden() {
    let object_types = object_types();
    let property_types = property_types();
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("the fixture corpus projects");
    assert_eq!(schema.object_tables.len(), object_types.len());
    let sql = schema.to_sql();

    let path = fixtures().join(GOLDEN);
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("golden directory");
        std::fs::write(&path, &sql).expect("golden writes");
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_default();
    if golden != sql {
        eprintln!("----BEGIN ACTUAL {GOLDEN}----\n{sql}----END ACTUAL----");
    }
    assert!(
        golden == sql,
        "{GOLDEN} is stale or missing; regenerate with {UPDATE_ENV}=1 cargo test -p norn-projections --test sqlite_ddl"
    );
}

/// The logical rendering is the physical DDL with every property column named
/// by its raw id (no corpus identifier otherwise contains `"p:`), and no
/// physical property column can equal a system column or SQLite rowid alias.
#[test]
fn property_columns_are_prefixed_and_the_logical_rendering_keeps_raw_ids() {
    let object_types = object_types();
    let property_types = property_types();
    let input = SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    };
    let physical = project_sqlite_ddl(&input).expect("physical projects");
    let logical = project_logical_sqlite_ddl(&input).expect("logical projects");
    assert_eq!(logical.to_sql(), physical.to_sql().replace("\"p:", "\""));
    for table in &physical.object_tables {
        for column in &table.columns {
            if let Some(name) = &column.column {
                assert_eq!(*name, format!("p:{}", column.property.as_str()));
            }
        }
    }

    let reserved = [
        "_world",
        "_id",
        "_type",
        "_version",
        "_scope",
        "_overflow",
        "_source",
        "_target",
        "rowid",
        "oid",
        "_rowid_",
    ];
    let properties: Vec<serde_json::Value> = reserved
        .iter()
        .map(|id| serde_json::json!({ "id": id, "type": { "kind": "text" } }))
        .collect();
    let def = decode_object_type(serde_json::json!({
        "name": "acme.Reserved",
        "properties": properties,
        "identity": [{ "name": "primary", "properties": ["rowid", "_id"] }]
    }));
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &[def],
        property_types: &[],
    })
    .expect("reserved property ids project");
    let table = &schema.object_tables[0];
    assert_eq!(table.columns.len(), reserved.len());
    for column in &table.columns {
        let name = column.column.as_deref().expect("physical column");
        assert!(
            name.starts_with("p:") && !reserved.contains(&name),
            "{name}"
        );
    }
    assert!(
        table.statements[1].ends_with("(\"_scope\", \"_world\", \"p:rowid\", \"p:_id\")"),
        "{}",
        table.statements[1]
    );
}

#[test]
fn the_projection_is_independent_of_input_order() {
    let object_types = object_types();
    let property_types = property_types();
    let forward = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("projects");
    let mut reversed_types = object_types;
    reversed_types.reverse();
    let mut reversed_properties = property_types;
    reversed_properties.reverse();
    let reversed = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &reversed_types,
        property_types: &reversed_properties,
    })
    .expect("projects");
    assert_eq!(forward, reversed);
}

#[test]
fn supertypes_precede_subtypes_and_subtypes_reference_them() {
    let object_types = object_types();
    let property_types = property_types();
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("projects");
    let position = |name: &str| {
        schema
            .object_tables
            .iter()
            .position(|table| table.object_type.as_str() == name)
            .expect("table")
    };
    assert!(position("crm.Customer") < position("crm.KeyAccount"));
    assert!(position("commerce.PurchaseOrder") < position("commerce.RushPurchaseOrder"));

    let rush = schema
        .object_table("commerce.RushPurchaseOrder")
        .expect("table");
    assert!(!rush.is_root());
    // The narrowing override of `status` stays in the supertype's column.
    assert!(
        rush.columns
            .iter()
            .all(|column| column.property.as_str() != "status")
    );
    assert!(rush.statements[0].contains(
        "FOREIGN KEY (\"_world\", \"_id\") REFERENCES \"o:commerce.PurchaseOrder\" (\"_world\", \"_id\") ON DELETE CASCADE"
    ));
}

#[test]
fn computed_properties_are_excluded_and_refs_resolve() {
    let object_types = object_types();
    let property_types = property_types();
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("projects");
    let line = schema
        .object_table("commerce.SalesOrderLine")
        .expect("table");
    assert!(
        line.columns
            .iter()
            .all(|column| column.property.as_str() != "line_total")
    );
    let currency = line
        .columns
        .iter()
        .find(|column| column.property.as_str() == "currency")
        .expect("currency");
    // commerce.price_currency → iso4217.currency_code → text.
    assert_eq!(currency.encoding, ValueEncoding::Text);
    let sku = schema.object_table("retail.Sku").expect("table");
    let weight = sku
        .columns
        .iter()
        .find(|column| column.property.as_str() == "net_weight")
        .expect("net_weight");
    assert_eq!(weight.encoding, ValueEncoding::Decimal);
    assert!(
        line.statements
            .iter()
            .any(|statement| statement.starts_with("CREATE UNIQUE INDEX IF NOT EXISTS \"o:commerce.SalesOrderLine/key:primary\" ON \"o:commerce.SalesOrderLine\" (\"_scope\", \"_world\", \"p:order_number\", \"p:line_number\")"))
    );
}

#[test]
fn link_tables_index_both_ends_and_enforce_single_ends() {
    let object_types = object_types();
    let property_types = property_types();
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("projects");
    // N:1 — one customer per order.
    let customer = schema
        .link_table("commerce.SalesOrder", "customer")
        .expect("link");
    assert!(customer.statements.iter().any(|statement| statement
        == "CREATE UNIQUE INDEX IF NOT EXISTS \"l:commerce.SalesOrder.customer/source\" ON \"l:commerce.SalesOrder.customer\" (\"_world\", \"_source\")"));
    assert!(customer.statements.iter().any(|statement| statement
        == "CREATE INDEX IF NOT EXISTS \"l:commerce.SalesOrder.customer/target\" ON \"l:commerce.SalesOrder.customer\" (\"_world\", \"_target\")"));
    // N:M with link properties.
    let members = schema
        .link_table("projects.Project", "members")
        .expect("link");
    assert_eq!(members.columns.len(), 2);
    assert!(
        members
            .statements
            .iter()
            .all(|statement| !statement.contains("/source\""))
    );
    // 1:1 — both ends unique.
    let account = schema
        .link_table("crm.BillingProfile", "account")
        .expect("link");
    assert_eq!(
        account
            .statements
            .iter()
            .filter(|statement| statement.starts_with("CREATE UNIQUE INDEX"))
            .count(),
        2
    );
}

fn decode_object_type(value: serde_json::Value) -> ObjectTypeDef {
    serde_json::from_value(value).expect("object type decodes")
}

#[test]
fn an_unknown_supertype_fails_closed() {
    let orphan = decode_object_type(serde_json::json!({
        "name": "acme.Orphan",
        "extends": "acme.Missing",
        "properties": [{ "id": "note", "type": { "kind": "text" } }]
    }));
    assert_eq!(
        project_sqlite_ddl(&SqliteDdlInput {
            object_types: &[orphan],
            property_types: &[],
        }),
        Err(SqliteDdlError::UnknownSupertype {
            object_type: "acme.Orphan".to_owned(),
            supertype: "acme.Missing".to_owned(),
        })
    );
}

#[test]
fn an_unresolved_ref_fails_closed() {
    let def = decode_object_type(serde_json::json!({
        "name": "acme.Thing",
        "properties": [{ "id": "code", "type": { "kind": "ref", "name": "acme.code" } }]
    }));
    assert_eq!(
        project_sqlite_ddl(&SqliteDdlInput {
            object_types: &[def],
            property_types: &[],
        }),
        Err(SqliteDdlError::UnresolvedPropertyType {
            owner: "acme.Thing".to_owned(),
            property: "code".to_owned(),
            name: "acme.code".to_owned(),
        })
    );
}

#[test]
fn a_subtype_key_over_inherited_properties_fails_closed() {
    let base = decode_object_type(serde_json::json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" } }]
    }));
    let derived = decode_object_type(serde_json::json!({
        "name": "acme.Derived",
        "extends": "acme.Base",
        "identity": [{ "name": "by_code", "properties": ["code"] }]
    }));
    assert_eq!(
        project_sqlite_ddl(&SqliteDdlInput {
            object_types: &[base, derived],
            property_types: &[],
        }),
        Err(SqliteDdlError::KeyNotLocal {
            object_type: "acme.Derived".to_owned(),
            key: "by_code".to_owned(),
            property: "code".to_owned(),
        })
    );
}

#[test]
fn duplicate_object_types_fail_closed() {
    let def = decode_object_type(serde_json::json!({ "name": "acme.Thing" }));
    assert_eq!(
        project_sqlite_ddl(&SqliteDdlInput {
            object_types: &[def.clone(), def],
            property_types: &[],
        }),
        Err(SqliteDdlError::DuplicateObjectType("acme.Thing".to_owned()))
    );
}

#[test]
fn a_wide_type_overflows_into_one_json_column() {
    let properties: Vec<serde_json::Value> = (0..120)
        .map(
            |index| serde_json::json!({ "id": format!("p{index}"), "type": { "kind": "integer" } }),
        )
        .collect();
    let def = decode_object_type(serde_json::json!({
        "name": "acme.Wide",
        "properties": properties,
        "identity": [{ "name": "primary", "properties": ["p0"] }]
    }));
    let schema = project_sqlite_ddl(&SqliteDdlInput {
        object_types: &[def],
        property_types: &[],
    })
    .expect("projects");
    let table = &schema.object_tables[0];
    assert!(table.has_overflow);
    let as_columns = table
        .columns
        .iter()
        .filter(|column| column.column.is_some())
        .count();
    // 5 system columns + the overflow column + the property columns.
    assert_eq!(as_columns + 6, MAX_COLUMNS_PER_TABLE);
    assert_eq!(table.columns.len(), 120);
    assert!(
        table.statements[0].contains(&format!("\"{OVERFLOW_COLUMN}\" TEXT")),
        "{}",
        table.statements[0]
    );
}
