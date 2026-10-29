#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::{
    BindingIndexSpec, CellError, CellStore, CommitBatch, CommitOutcome, DefinitionReadmission,
    FactWriteRow, InstalledDefinitionInput, LinkChange, MAIN_WORLD, Model, StoreOptions,
};
use norn_ir::{InterfaceDef, RuntimeValue as Value};
use norn_kernel::OWNED_ROWS_TYPE;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, SCOPE_COLUMN, TYPE_COLUMN, VERSION_COLUMN, WORLD_COLUMN,
    quote_identifier,
};
use serde_json::json;

fn model(with_optional_property: bool) -> Model {
    let mut properties = vec![json!({"id":"name","type":{"kind":"text"}})];
    if with_optional_property {
        properties.push(json!({
            "id":"note",
            "type":{"kind":"text"},
            "constraints":{"nullable":true}
        }));
    }
    let definition = serde_json::from_value(json!({
        "name":"defs.Item",
        "properties":properties
    }))
    .expect("object type decodes");
    Model::from_definitions(&[definition], &[]).expect("model projects")
}

fn parent_rows_migration_model(owned: bool) -> Model {
    let mut parent = json!({
        "name":"defs.Parent",
        "properties":[{"id":"title", "type":{"kind":"text"}}],
        "links":[{
            "id":"lines",
            "target":"defs.Line",
            "cardinality":{"to":"many", "from":"one"}
        }]
    });
    if owned {
        parent["ownedCollections"] = json!([{
            "id":"lines",
            "childType":"defs.Line",
            "rowIdProperty":"row_id",
            "orderingKey":"idx",
            "cascadeOnParentDelete":true,
            "maxRows":10
        }]);
    }
    model_from_json(vec![
        parent,
        json!({
            "name":"defs.Line",
            "properties":[
                {"id":"row_id", "type":{"kind":"text"}},
                {"id":"idx", "type":{"kind":"integer"}},
                {"id":"label", "type":{"kind":"text"}}
            ]
        }),
        json!({
            "name":"norn.OwnedRows",
            "properties":[
                {"id":"parentType", "type":{"kind":"text"}},
                {"id":"parentKey", "type":{"kind":"text"}},
                {"id":"collectionId", "type":{"kind":"text"}},
                {"id":"rowIds", "type":{"kind":"json-ext"}}
            ]
        }),
    ])
}

fn model_with_name_binding_index(model: &Model) -> Model {
    let object_types = model.object_types().cloned().collect::<Vec<_>>();
    let interfaces = model.interfaces().cloned().collect::<Vec<_>>();
    Model::from_definitions_with_binding_indexes(
        &object_types,
        model.property_types(),
        &interfaces,
        &[BindingIndexSpec {
            object_type: "defs.Item".to_owned(),
            property_id: "name".to_owned(),
            sort: true,
            filter: true,
        }],
    )
    .expect("binding index model projects")
}

fn admission_receipt(checker_version: &str, expr_profile: &str) -> (String, String) {
    let value = Value::from_json(json!({
        "format": "norn-checker/receipt@1",
        "checker": { "name": "norn-checker", "version": checker_version },
        "artifactSet": { "artifacts": [], "base": [], "sha256": "0".repeat(64) },
        "limitProfile": { "id": "norn.limits", "version": 1, "sha256": "0".repeat(64) },
        "exprProfile": expr_profile,
        "diagnostics": { "count": 0, "errors": 0, "sha256": "0".repeat(64) },
        "admitted": true
    }))
    .expect("receipt is exact JSON");
    let receipt = String::from_utf8(value.canonical_json().expect("receipt canonicalizes"))
        .expect("canonical receipt is UTF-8");
    let digest = norn_checker::admission_receipt_sha256(&receipt).expect("receipt hashes");
    (receipt, digest)
}

fn model_from_json(definitions: Vec<serde_json::Value>) -> Model {
    let definitions: Vec<norn_ir::entity_v2::object::ObjectTypeDef> = definitions
        .into_iter()
        .map(|definition| serde_json::from_value(definition).expect("object type decodes"))
        .collect();
    Model::from_definitions(&definitions, &[]).expect("model projects")
}

fn total_invariant_model(
    include_total: bool,
    default_total: Option<i64>,
    with_invariant: bool,
) -> Model {
    let mut properties = vec![json!({"id":"name", "type":{"kind":"text"}})];
    if include_total {
        let mut total = json!({
            "id":"total",
            "type":{"kind":"integer"}
        });
        if let Some(default) = default_total {
            total["default"] = json!(default.to_string());
        }
        properties.push(total);
    }
    let mut definition = json!({"name":"defs.Item", "properties":properties});
    if with_invariant {
        definition["invariants"] = json!([{
            "id":"non_negative_total",
            "expression":"self.total >= 0",
            "message":"total must be non-negative"
        }]);
    }
    model_from_json(vec![definition])
}

fn temporal_default_model(timestamp: &str) -> Model {
    model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {
                "id":"occurred_at",
                "type":{"kind":"timestamp", "timezone":"utc"},
                "default":timestamp
            }
        ]
    })])
}

fn temporal_contract_model(with_invariant: bool) -> Model {
    let mut definition = json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {
                "id":"service_date",
                "type":{"kind":"date"},
                "constraints":{"nullable":true}
            }
        ]
    });
    if with_invariant {
        definition["invariants"] = json!([{
            "id":"migration_gate",
            "expression":"true",
            "message":"the candidate invariant is satisfied"
        }]);
    }
    model_from_json(vec![definition])
}

fn name_invariant_model(expression: &str) -> Model {
    model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "invariants":[{
            "id":"name_invariant",
            "expression":expression,
            "message":"name invariant must hold"
        }]
    })])
}

fn name_quality_invariant_model(expression: &str) -> Model {
    model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "invariants":[{
            "id":"name_invariant",
            "classification":"quality",
            "expression":expression,
            "message":"name is a quality expectation"
        }]
    })])
}

fn inherited_total_invariant_model(with_invariant: bool) -> Model {
    let mut base = json!({
        "name":"defs.Base",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"total", "type":{"kind":"integer"}}
        ]
    });
    if with_invariant {
        base["invariants"] = json!([{
            "id":"non_negative_total",
            "expression":"self.total >= 0",
            "message":"total must be non-negative"
        }]);
    }
    model_from_json(vec![
        base,
        json!({"name":"defs.Child", "extends":"defs.Base"}),
    ])
}

fn interface_total_invariant_model(with_invariant: bool) -> Model {
    let object_type = serde_json::from_value(json!({
        "name":"defs.Item",
        "implements":["defs.NonnegativeTotal"],
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"total", "type":{"kind":"integer"}}
        ]
    }))
    .expect("object type decodes");
    let mut interface = json!({
        "name":"defs.NonnegativeTotal",
        "properties":[{"id":"total", "type":{"kind":"integer"}}]
    });
    if with_invariant {
        interface["invariants"] = json!([{
            "id":"non_negative_total",
            "expression":"self.total >= 0",
            "message":"total must be non-negative"
        }]);
    }
    let interface: InterfaceDef = serde_json::from_value(interface).expect("interface decodes");
    Model::from_definitions_with_interfaces(&[object_type], &[], &[interface])
        .expect("interface model projects")
}

fn integer_value(value: i64) -> Value {
    Value::from_json_text(value.to_string()).expect("integer is an exact runtime value")
}

fn item_type(properties: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"name":"defs.Item", "properties":serde_json::Value::Array(properties)})
}

fn lifecycle_model(remove_inherited_state: bool) -> Model {
    let status_values = if remove_inherited_state {
        json!([{"value":"draft"}, {"value":"done"}])
    } else {
        json!([{"value":"draft"}, {"value":"active"}, {"value":"done"}])
    };
    let transitions = if remove_inherited_state {
        json!([{"id":"finish", "from":["draft"], "to":"done"}])
    } else {
        json!([
            {"id":"activate", "from":["draft"], "to":"active"},
            {"id":"finish", "from":["active"], "to":"done"}
        ])
    };
    model_from_json(vec![
        json!({
            "name":"defs.Base",
            "properties":[
                {"id":"name", "type":{"kind":"text"}},
                {"id":"status", "type":{"kind":"enum", "values":status_values}}
            ],
            "lifecycles":[{
                "property":"status", "initial":["draft"], "terminal":["done"],
                "transitions":transitions
            }]
        }),
        json!({"name":"defs.Child", "extends":"defs.Base"}),
    ])
}

fn overflow_model(add_boolean_default: bool) -> Model {
    let mut properties = vec![json!({"id":"name", "type":{"kind":"text"}})];
    properties.extend((0..110).map(|index| {
        json!({
            "id":format!("field_{index}"),
            "type":{"kind":"text"},
            "constraints":{"nullable":true}
        })
    }));
    if add_boolean_default {
        properties.push(json!({
            "id":"enabled",
            "type":{"kind":"boolean"},
            "default":true
        }));
    }
    model_from_json(vec![item_type(properties)])
}

fn rowid_overflow_model(add_migration_properties: bool) -> Model {
    let mut properties = vec![json!({"id":"name", "type":{"kind":"text"}})];
    properties.extend((0..92).map(|index| {
        json!({
            "id":format!("field_{index}"),
            "type":{"kind":"text"},
            "constraints":{"nullable":true}
        })
    }));
    if add_migration_properties {
        properties.extend([
            json!({"id":"rowid", "type":{"kind":"text"}, "default":"property-rowid"}),
            json!({"id":"enabled", "type":{"kind":"boolean"}, "default":true}),
            json!({"id":"extra", "type":{"kind":"text"}, "constraints":{"nullable":true}}),
        ]);
    }
    model_from_json(vec![item_type(properties)])
}

fn enum_model(values: &[&str]) -> Model {
    let values = values
        .iter()
        .map(|value| json!({"value":value}))
        .collect::<Vec<_>>();
    model_from_json(vec![item_type(vec![
        json!({"id":"name", "type":{"kind":"text"}}),
        json!({"id":"status", "type":{"kind":"enum", "values":values}}),
    ])])
}

fn inherited_enum_model(values: &[&str]) -> Model {
    let values = values
        .iter()
        .map(|value| json!({"value":value}))
        .collect::<Vec<_>>();
    model_from_json(vec![
        json!({
            "name":"defs.Base",
            "properties":[
                {"id":"name", "type":{"kind":"text"}},
                {"id":"status", "type":{"kind":"enum", "values":values}}
            ]
        }),
        json!({"name":"defs.Child", "extends":"defs.Base"}),
    ])
}

fn overflow_enum_model(values: &[&str]) -> Model {
    let mut properties = (0..110)
        .map(|index| {
            json!({
                "id":format!("field_{index}"),
                "type":{"kind":"text"},
                "constraints":{"nullable":true}
            })
        })
        .collect::<Vec<_>>();
    let values = values
        .iter()
        .map(|value| json!({"value":value}))
        .collect::<Vec<_>>();
    properties.extend([
        json!({"id":"name", "type":{"kind":"text"}}),
        json!({"id":"status", "type":{"kind":"enum", "values":values}}),
    ]);
    model_from_json(vec![item_type(properties)])
}

fn link_enum_model(values: &[&str]) -> Model {
    let values = values
        .iter()
        .map(|value| json!({"value":value}))
        .collect::<Vec<_>>();
    model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "links":[{
            "id":"next", "target":"defs.Item",
            "cardinality":{"to":"many", "from":"many"},
            "properties":[{"id":"role", "type":{"kind":"enum", "values":values}}]
        }]
    })])
}

fn required_link_model() -> Model {
    model_from_json(vec![
        json!({
            "name":"defs.Source",
            "properties":[{"id":"name", "type":{"kind":"text"}}],
            "links":[{
                "id":"target",
                "target":"defs.Target",
                "cardinality":{"to":"many", "from":"many", "required":true}
            }]
        }),
        json!({
            "name":"defs.Target",
            "properties":[{"id":"name", "type":{"kind":"text"}}]
        }),
    ])
}

fn interface_link_model(customer_implements: bool, other_implements: bool) -> Model {
    let customer_implements = if customer_implements {
        json!(["party.Party"])
    } else {
        json!([])
    };
    let other_implements = if other_implements {
        json!(["party.Party"])
    } else {
        json!([])
    };
    let definitions: Vec<norn_ir::entity_v2::object::ObjectTypeDef> = vec![
        json!({
            "name":"crm.Customer",
            "implements":customer_implements,
            "properties":[{"id":"name", "type":{"kind":"text"}}]
        }),
        json!({
            "name":"crm.Other",
            "implements":other_implements,
            "properties":[{"id":"name", "type":{"kind":"text"}}]
        }),
        json!({
            "name":"sales.Order",
            "properties":[{"id":"name", "type":{"kind":"text"}}],
            "links":[{
                "id":"customer",
                "target":"party.Party",
                "cardinality":{"to":"many", "from":"many"}
            }]
        }),
    ]
    .into_iter()
    .map(|definition| serde_json::from_value(definition).expect("object type decodes"))
    .collect();
    let party_interface: InterfaceDef = serde_json::from_value(json!({
        "name":"party.Party",
        "properties":[]
    }))
    .expect("party interface decodes");
    Model::from_definitions_with_interfaces(&definitions, &[], &[party_interface])
        .expect("interface link model projects")
}

fn decimal_unit_model(unit: &str) -> Model {
    model_from_json(vec![item_type(vec![
        json!({"id":"name", "type":{"kind":"text"}}),
        json!({
            "id":"amount",
            "type":{"kind":"decimal", "scale":2, "unit":{"system":"ucum", "code":unit}}
        }),
    ])])
}

fn install_input(
    version_label: &str,
    installed_at: u64,
    model: &Model,
) -> InstalledDefinitionInput {
    let mut artifacts: Vec<serde_json::Value> = model
        .object_types()
        // The projected test model includes the host-owned manifest table,
        // but installed definition sets contain admitted user artifacts only.
        // In particular, internal camelCase storage fields must not be
        // mislabeled as EntityIR property ids.
        .filter(|object_type| object_type.name.as_str() != OWNED_ROWS_TYPE)
        .map(|object_type| {
            let type_name = object_type.name.as_str();
            let (namespace, name) = type_name
                .rsplit_once('.')
                .unwrap_or(("definitions_test", type_name));
            json!({
                "irVersion": "0.1",
                "artifactId": format!("{type_name}.entity"),
                "kind": "entity",
                "namespace": namespace,
                "name": name,
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": {"orgId": "org_demo", "environmentId": "dev"},
                "dependencies": [],
                "provenance": {"source": "authoring_sdk"},
                "payload": {"entityVersion": "2.0", "objectType": object_type}
            })
        })
        .collect();
    artifacts.extend(model.interfaces().map(|interface| {
        let type_name = interface.name.as_str();
        let (namespace, name) = type_name
            .rsplit_once('.')
            .unwrap_or(("definitions_test", type_name));
        json!({
            "irVersion": "0.1",
            "artifactId": format!("{type_name}.interface"),
            "kind": "entity",
            "namespace": namespace,
            "name": name,
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [],
            "provenance": {"source": "authoring_sdk"},
            "payload": {"entityVersion": "2.0", "interface": interface}
        })
    }));
    let canonical_set = String::from_utf8(
        Value::from_json(json!(artifacts))
            .expect("artifact set is exact JSON")
            .canonical_json()
            .expect("artifact set canonicalizes"),
    )
    .expect("canonical artifact set is UTF-8");
    InstalledDefinitionInput {
        canonical_set,
        registry: Vec::new(),
        receipt_sha256: format!("receipt-{version_label}"),
        admission_receipt: None,
        readmission: None,
        diff: json!({"added":{"count":1,"ids":[version_label]}}),
        installed_at,
        layout: None,
    }
}

fn layout_sidecar(x: i64) -> String {
    let value = Value::from_json(json!({
        "layoutVersion": "0.1",
        "artifacts": {
            "defs.Item.entity": {
                "nodes": {
                    "node-1": { "position": { "x": x, "y": 2 } }
                }
            }
        }
    }))
    .expect("layout sidecar is exact JSON");
    String::from_utf8(value.canonical_json().expect("layout canonicalizes"))
        .expect("layout is UTF-8")
}

fn put(name: &str, note: Option<&str>) -> FactWriteRow {
    put_type("defs.Item", "item-1", name, note)
}

fn put_type(object_type: &str, key: &str, name: &str, note: Option<&str>) -> FactWriteRow {
    let mut values = BTreeMap::from([("name".to_owned(), Value::String(name.to_owned()))]);
    if let Some(note) = note {
        values.insert("note".to_owned(), Value::String(note.to_owned()));
    }
    FactWriteRow::Put {
        object_type: object_type.to_owned(),
        key: key.to_owned(),
        values,
    }
}

fn put_total(object_type: &str, key: &str, total: i64) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: object_type.to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            ("name".to_owned(), Value::String(key.to_owned())),
            ("total".to_owned(), integer_value(total)),
        ]),
    }
}

fn batch(write: FactWriteRow, logical_time: u64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time,
        writes: vec![write],
        ..CommitBatch::default()
    }
}

/// Add large migration-budget fixture rows without sending huge values through
/// the commit and canonical-value paths. The extra projected rows intentionally
/// have no ledger/export records; migration reads only their SQLite projection.
struct MigrationBudgetRows<'a> {
    path: &'a Path,
    model: &'a Model,
    store: &'a mut CellStore,
    world: &'a str,
    row_ids: &'a [String],
    value_column: &'a str,
    value: String,
    distinct_names: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "the synthetic fixture copies one valid projection and its identity versions atomically"
)]
fn seed_migration_budget_rows(fixture: MigrationBudgetRows<'_>) -> NativeEngine {
    let MigrationBudgetRows {
        path,
        model,
        store,
        world,
        row_ids,
        value_column,
        value,
        distinct_names,
    } = fixture;
    let seed_id = row_ids.first().expect("migration fixture has a seed id");
    let mut seed = batch(put_type("defs.Item", seed_id, "seed", None), 2);
    seed.world = world.to_owned();
    store.commit(&seed).expect("small seed row commits");

    let table = model
        .schema()
        .object_table("defs.Item")
        .expect("fixture object table projects");
    assert!(table.is_root(), "fixture table is a root table");
    let mut columns = vec![
        WORLD_COLUMN.to_owned(),
        ID_COLUMN.to_owned(),
        TYPE_COLUMN.to_owned(),
        SCOPE_COLUMN.to_owned(),
        VERSION_COLUMN.to_owned(),
    ];
    columns.extend(
        table
            .columns
            .iter()
            .filter_map(|column| column.column.clone()),
    );
    if table.has_overflow {
        columns.push(OVERFLOW_COLUMN.to_owned());
    }
    let quoted_columns = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let quoted_table = quote_identifier(&table.table);
    let update = format!(
        "UPDATE {quoted_table} SET {} = ?1 WHERE {} = ?2 AND {} = ?3",
        quote_identifier(value_column),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );

    let mut insert_values = Vec::with_capacity(columns.len());
    for column in &columns {
        if column == ID_COLUMN {
            insert_values.push("?1".to_owned());
        } else if distinct_names && column == "name" {
            insert_values.push(format!("{} || ?2", quote_identifier(column)));
        } else {
            insert_values.push(quote_identifier(column));
        }
    }
    let insert_values = insert_values.join(", ");
    let (world_parameter, source_parameter) = if distinct_names {
        ("?3", "?4")
    } else {
        ("?2", "?3")
    };
    let insert = format!(
        "INSERT INTO {quoted_table} ({quoted_columns}) SELECT {insert_values} FROM {quoted_table} WHERE {} = {world_parameter} AND {} = {source_parameter}",
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );
    let duplicate_version = r#"
        INSERT INTO "_norn_version" ("world", "root_type", "id", "version")
        SELECT "world", "root_type", ?1, "version" FROM "_norn_version"
        WHERE "world" = ?2 AND "root_type" = ?3 AND "id" = ?4
    "#;
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("fixture engine opens");
    engine
        .transaction(|| -> Result<(), CellError> {
            engine.execute(
                &update,
                &[
                    SqlValue::Text(value),
                    SqlValue::Text(world.to_owned()),
                    SqlValue::Text(seed_id.clone()),
                ],
            )?;
            for id in row_ids.iter().skip(1) {
                let mut parameters = vec![SqlValue::Text(id.clone())];
                if distinct_names {
                    parameters.push(SqlValue::Text(format!("-{id}")));
                }
                parameters.push(SqlValue::Text(world.to_owned()));
                parameters.push(SqlValue::Text(seed_id.clone()));
                engine.execute(&insert, &parameters)?;
                engine.execute(
                    duplicate_version,
                    &[
                        SqlValue::Text(id.clone()),
                        SqlValue::Text(world.to_owned()),
                        SqlValue::Text(table.object_type.as_str().to_owned()),
                        SqlValue::Text(seed_id.clone()),
                    ],
                )?;
            }
            Ok(())
        })
        .expect("large fixture rows seed in one SQL transaction");
    engine
}

fn migration_budget_column_bytes(
    engine: &NativeEngine,
    model: &Model,
    column: &str,
    world: &str,
) -> (i64, i64) {
    let table = model
        .schema()
        .object_table("defs.Item")
        .expect("fixture object table projects");
    let sql = format!(
        "SELECT COALESCE(SUM(octet_length({})), 0), COALESCE(MAX(octet_length({})), 0) FROM {} WHERE {} = ?1",
        quote_identifier(column),
        quote_identifier(column),
        quote_identifier(&table.table),
        quote_identifier(WORLD_COLUMN),
    );
    engine
        .query_row(&sql, &[SqlValue::Text(world.to_owned())], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("fixture byte lengths query")
}

fn migration_budget_version_snapshot(engine: &NativeEngine, world: &str) -> (i64, i64) {
    engine
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(version), 0) FROM \"_norn_version\" WHERE \"world\" = ?1",
            &[SqlValue::Text(world.to_owned())],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("fixture versions query")
}

#[test]
fn verified_definition_history_preserves_predecessors_without_raw_metadata() {
    let original = model(false);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    for version in 1..=3 {
        store
            .install_definitions(
                original.clone(),
                install_input(&format!("v{version}"), version, &original),
                Some(version.checked_sub(1).expect("positive install version")),
                "org_demo/dev",
            )
            .expect("consecutive install");
    }
    let reader = store.read();
    let entries = reader.ledger(0).expect("retained install history");
    assert_eq!(entries.len(), 3);
    for (entry, previous) in entries.iter().zip([None, Some(1), Some(2)]) {
        let projected = reader
            .verified_history_event(entry)
            .expect("verified history")
            .expect("typed install is proven");
        assert_eq!(
            projected.get("previousVersion"),
            Some(&Value::from_json(json!(previous)).expect("runtime predecessor"))
        );
        assert_eq!(projected.get("version"), entry.event.get("version"));
        assert_eq!(
            projected.get("receiptSha256"),
            entry.event.get("receiptSha256")
        );
        assert!(entry.event.get("diff").is_some());
        assert!(projected.get("diff").is_none());

        let mut untrusted = entry.clone();
        let raw = untrusted.event.as_object_mut().expect("event object");
        raw.insert("previousVersion".to_owned(), Value::from(999));
        raw.insert("version".to_owned(), Value::from(999));
        raw.insert("receiptSha256".to_owned(), Value::from("unproven"));
        raw.insert("diff".to_owned(), Value::from("unproven"));
        assert_eq!(
            reader
                .verified_history_event(&untrusted)
                .expect("authenticate historical fields"),
            Some(projected),
            "raw wrapper claims cannot change receipt-bound install fields"
        );
        untrusted.commit_seq = untrusted.commit_seq.checked_add(1).expect("next commit");
        assert!(
            reader
                .verified_history_event(&untrusted)
                .expect("mismatched commit")
                .is_none()
        );
    }
}

#[test]
fn definition_install_migrates_additive_columns_and_retains_versioned_history() {
    let original = model(false);
    let changed = model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");

    let first = store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed v1 installs when the active version is zero");
    assert_eq!(first.version, 1);
    assert_eq!(first.commit_seq, 1);

    let CommitOutcome::Committed(created) =
        store.commit(&batch(put("Alpha", None), 2)).expect("create")
    else {
        panic!("create commits")
    };
    assert_eq!(created.body.definitions_version, 1);

    let second = store
        .install_definitions(
            changed.clone(),
            install_input("v2", 3, &changed),
            Some(1),
            "org_demo/dev",
        )
        .expect("additive optional property installs");
    assert_eq!(second.version, 2);
    assert_eq!(second.commit_seq, 3);
    assert_eq!(
        store.read().installed_definitions(None).expect("active"),
        Some(second)
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(Some(1))
            .expect("retained v1"),
        Some(first)
    );
    assert_eq!(
        store.read().definitions_version_at(2).expect("v1 in force"),
        Some(1)
    );
    assert_eq!(
        store.read().definitions_version_at(3).expect("v2 in force"),
        Some(2)
    );

    let event = store.read().ledger(0).expect("ledger");
    assert_eq!(
        event[0].event.get("type").and_then(Value::as_str),
        Some("definitions.installed")
    );
    assert_eq!(
        event[0].event.get("version").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        event[1].event.get("type").and_then(Value::as_str),
        Some("definitions.installed")
    );
    assert_eq!(
        event[1].event.get("version").and_then(Value::as_i64),
        Some(2)
    );

    store
        .commit(&batch(put("Alpha", Some("migrated")), 4))
        .expect("new property writes");
    let object = store
        .read()
        .object("main", "defs.Item", "item-1")
        .expect("read")
        .expect("object remains readable");
    assert_eq!(
        object.values.get("note").and_then(Value::as_str),
        Some("migrated")
    );
    let stale = model(false);
    assert!(matches!(
        store.install_definitions(
            stale.clone(),
            install_input("v3", 5, &stale),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionVersionConflict {
            expected: Some(1),
            actual: Some(2)
        })
    ));
}

#[test]
fn layout_snapshots_have_independent_revisions_and_share_definition_version_cas() {
    let original = model(false);
    let expanded = model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    let first_layout = layout_sidecar(10);
    let mut first_input = install_input("layout-v1", 1, &original);
    first_input.layout = Some(first_layout.clone());
    let first = store
        .install_definitions(original.clone(), first_input, Some(0), "org_demo/dev")
        .expect("initial sidecar installs");
    assert_eq!(first.layout.as_deref(), Some(first_layout.as_str()));
    assert_eq!(first.layout_revision, 1);

    // A semantic definition change with an omitted sidecar inherits the exact
    // prior bytes and does not advance the independent layout revision.
    let second = store
        .install_definitions(
            expanded.clone(),
            install_input("layout-v2", 2, &expanded),
            Some(1),
            "org_demo/dev",
        )
        .expect("omitted layout preserves the prior snapshot");
    assert_eq!(second.layout.as_deref(), Some(first_layout.as_str()));
    assert_eq!(second.layout_revision, 1);

    let mut same_layout_input = install_input("layout-v3", 3, &expanded);
    same_layout_input.layout = Some(first_layout.clone());
    let third = store
        .install_definitions(expanded.clone(), same_layout_input, Some(2), "org_demo/dev")
        .expect("same snapshot is a no-op for its revision");
    assert_eq!(third.layout.as_deref(), Some(first_layout.as_str()));
    assert_eq!(third.layout_revision, 1);

    let changed_layout = layout_sidecar(20);
    let mut changed_input = install_input("layout-v4", 4, &expanded);
    changed_input.layout = Some(changed_layout.clone());
    let fourth = store
        .install_definitions(expanded, changed_input, Some(3), "org_demo/dev")
        .expect("geometry update uses the expected definitions version");
    assert_eq!(fourth.layout.as_deref(), Some(changed_layout.as_str()));
    assert_eq!(fourth.layout_revision, 2);
    assert_eq!(
        store
            .read()
            .installed_definitions(Some(1))
            .expect("historical layout")
            .expect("version one")
            .layout
            .as_deref(),
        Some(first_layout.as_str())
    );
    assert!(matches!(
        store.install_definitions(
            original.clone(),
            install_input("stale", 5, &original),
            Some(3),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionVersionConflict {
            expected: Some(3),
            actual: Some(4)
        })
    ));
}

#[test]
fn binding_sort_index_installs_and_retires_with_live_rows() {
    let original = model(false);
    let indexed = model_with_name_binding_index(&original);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("binding-index-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("base definitions install");
    store
        .commit(&batch(put("Alpha", None), 2))
        .expect("create a live row before index install");

    store
        .install_definitions(
            indexed.clone(),
            install_input("binding-index-v2", 3, &indexed),
            Some(1),
            "org_demo/dev",
        )
        .expect("binding index installs over live rows");
    let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    let mut budget = DecodeBudget::new(&limits);
    let candidate_bytes = budget.remaining_bytes();
    let candidate = store
        .read()
        .binding_sorted_candidate_after_with_budget(
            MAIN_WORLD,
            "defs.Item",
            "name",
            false,
            None,
            &mut budget,
            candidate_bytes,
        )
        .expect("installed sort index is usable")
        .expect("live candidate fits");
    assert_eq!(
        candidate.identity,
        Some(("item-1".to_owned(), "defs.Item".to_owned()))
    );

    store
        .install_definitions(
            original.clone(),
            install_input("binding-index-v3", 4, &original),
            Some(2),
            "org_demo/dev",
        )
        .expect("optional binding index can be removed with live rows");
}

#[test]
fn removing_unique_key_named_for_binding_property_still_refuses_live_rows() {
    let with_key = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "identity":[{
            "name":"binding_property_name",
            "properties":["name"]
        }]
    })]);
    let without_key = model(false);
    let mut store = CellStore::open_in_memory(with_key.clone()).expect("store opens");
    store
        .install_definitions(
            with_key.clone(),
            install_input("binding-name-key-v1", 1, &with_key),
            Some(0),
            "org_demo/dev",
        )
        .expect("key definitions install");
    store
        .commit(&batch(put("Alpha", None), 2))
        .expect("create a live row");

    assert!(matches!(
        store.install_definitions(
            without_key.clone(),
            install_input("binding-name-key-v2", 3, &without_key),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
}

#[test]
fn definition_install_canonicalizes_timestamp_defaults_for_existing_rows() {
    let original = model(false);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("temporal-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("base definitions install");
    store
        .commit(&batch(put("Alpha", None), 2))
        .expect("existing object commits");

    let candidate = temporal_default_model("2024-02-29T12:30:00+02:30");
    store
        .install_definitions(
            candidate.clone(),
            install_input("temporal-v2", 3, &candidate),
            Some(1),
            "org_demo/dev",
        )
        .expect("explicit offset timestamp default is admitted and normalized");
    let object = store
        .read()
        .object(MAIN_WORLD, "defs.Item", "item-1")
        .expect("object read succeeds")
        .expect("existing object remains live");
    assert_eq!(
        object.values.get("occurred_at").and_then(Value::as_str),
        Some("2024-02-29T10:00:00Z")
    );
}

#[test]
fn changed_invariant_scans_temporal_only_contracts_for_invalid_live_values() {
    for (date, should_install) in [("2025-02-29", false), ("2024-02-29", true)] {
        let original = temporal_contract_model(false);
        let candidate = temporal_contract_model(true);
        let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
        store
            .install_definitions(
                original.clone(),
                install_input("temporal-check-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("original temporal contract installs");
        store
            .commit(&batch(
                FactWriteRow::Put {
                    object_type: "defs.Item".to_owned(),
                    key: "item-1".to_owned(),
                    values: BTreeMap::from([
                        ("name".to_owned(), Value::String("Item".to_owned())),
                        ("service_date".to_owned(), Value::String(date.to_owned())),
                    ]),
                },
                2,
            ))
            .expect("low-level legacy row is stored for migration validation");
        let before = store.read().verify_ledger().expect("ledger before install");

        let result = store.install_definitions(
            candidate.clone(),
            install_input("temporal-check-v2", 3, &candidate),
            Some(1),
            "org_demo/dev",
        );
        if should_install {
            result.expect("valid temporal value allows candidate invariant install");
        } else {
            let refusal = format!("{result:?}");
            let Err(CellError::DefinitionInvariantViolation {
                violations,
                truncated,
            }) = result
            else {
                panic!("invalid temporal value must refuse the candidate install: {refusal}")
            };
            assert!(!truncated);
            assert_eq!(violations.len(), 1);
            assert_eq!(violations[0].object.object_type, "defs.Item");
            assert_eq!(violations[0].object.id, "item-1");
            assert_eq!(violations[0].id.as_str(), "$property.service_date");
            assert_eq!(
                violations[0].outcome,
                norn_ir::DataQualityOutcome::NotSatisfied
            );
            assert_eq!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("active definitions remain readable")
                    .map(|installed| installed.version),
                Some(1)
            );
            assert_eq!(
                store.read().verify_ledger().expect("ledger after refusal"),
                before
            );
        }
    }
}

#[test]
fn removing_a_property_with_live_values_refuses_the_migration() {
    let original = model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    store
        .commit(&batch(put("Alpha", Some("live")), 2))
        .expect("create");

    let candidate = model(false);
    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(
        store
            .read()
            .verify_ledger()
            .expect("ledger verifies")
            .commits,
        2
    );
}

#[test]
fn removing_a_type_with_live_rows_refuses_without_committing() {
    let original = model_from_json(vec![
        item_type(vec![json!({"id":"name", "type":{"kind":"text"}})]),
        json!({
            "name":"defs.Other",
            "properties":[{"id":"name", "type":{"kind":"text"}}]
        }),
    ]);
    let candidate = model(false);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    store
        .commit(&batch(put_type("defs.Other", "other-1", "Other", None), 2))
        .expect("create live other row");

    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 2);
}

#[test]
fn removing_a_link_with_live_rows_refuses_without_committing() {
    let linked = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "links":[{
            "id":"next", "target":"defs.Item",
            "cardinality":{"to":"many", "from":"many"}
        }]
    })]);
    let unlinked = model(false);
    let mut store = CellStore::open_in_memory(linked.clone()).expect("store opens");
    store
        .install_definitions(
            linked.clone(),
            install_input("v1", 1, &linked),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![
                put_type("defs.Item", "item-1", "One", None),
                put_type("defs.Item", "item-2", "Two", None),
            ],
            links: vec![LinkChange::Add {
                object_type: "defs.Item".to_owned(),
                link: "next".to_owned(),
                source: "item-1".to_owned(),
                target: "item-2".to_owned(),
                values: BTreeMap::new(),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("create live link");

    assert!(matches!(
        store.install_definitions(
            unlinked.clone(),
            install_input("v2", 3, &unlinked),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 2);
}

#[test]
fn adding_owned_collection_refuses_preexisting_child_facts_without_manifest_migration() {
    let legacy = parent_rows_migration_model(false);
    let candidate = parent_rows_migration_model(true);
    let mut store = CellStore::open_in_memory(legacy.clone()).expect("store opens");
    store
        .install_definitions(
            legacy.clone(),
            install_input("owned-rows-v1", 1, &legacy),
            Some(0),
            "org_demo/dev",
        )
        .expect("legacy definitions install");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![FactWriteRow::Put {
                object_type: "defs.Line".to_owned(),
                key: "line-1".to_owned(),
                values: BTreeMap::from([
                    ("row_id".to_owned(), Value::String("line-1".to_owned())),
                    ("idx".to_owned(), integer_value(1)),
                    ("label".to_owned(), Value::String("existing".to_owned())),
                ]),
            }],
            ..CommitBatch::default()
        })
        .expect("an unowned child fact can predate the collection declaration");

    let input = install_input("owned-rows-v2", 3, &candidate);
    let before = store.read().verify_ledger().expect("ledger before check");
    assert!(matches!(
        store.check_definitions(&candidate, &input, Some(1)),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert!(matches!(
        store.install_definitions(candidate.clone(), input, Some(1), "org_demo/dev"),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active definitions")
            .map(|installed| installed.version),
        Some(1)
    );

    let mut empty_store = CellStore::open_in_memory(legacy.clone()).expect("empty store opens");
    empty_store
        .install_definitions(
            legacy.clone(),
            install_input("owned-rows-empty-v1", 1, &legacy),
            Some(0),
            "org_demo/dev",
        )
        .expect("empty legacy definitions install");
    empty_store
        .install_definitions(
            candidate.clone(),
            install_input("owned-rows-empty-v2", 2, &candidate),
            Some(1),
            "org_demo/dev",
        )
        .expect("ownership can be introduced while affected live data is absent");
}

#[test]
fn removing_owned_collection_refuses_live_child_rows_and_parent_links() {
    let previous = parent_rows_migration_model(true);
    let candidate = parent_rows_migration_model(false);
    let mut store = CellStore::open_in_memory(previous.clone()).expect("store opens");
    store
        .install_definitions(
            previous.clone(),
            install_input("owned-rows-remove-v1", 1, &previous),
            Some(0),
            "org_demo/dev",
        )
        .expect("owned collection installs");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![
                FactWriteRow::Put {
                    object_type: "defs.Parent".to_owned(),
                    key: "parent-1".to_owned(),
                    values: BTreeMap::from([(
                        "title".to_owned(),
                        Value::String("order".to_owned()),
                    )]),
                },
                FactWriteRow::Put {
                    object_type: "defs.Line".to_owned(),
                    key: "line-1".to_owned(),
                    values: BTreeMap::from([
                        ("row_id".to_owned(), Value::String("line-1".to_owned())),
                        ("idx".to_owned(), integer_value(1)),
                        ("label".to_owned(), Value::String("item".to_owned())),
                    ]),
                },
            ],
            links: vec![LinkChange::Add {
                object_type: "defs.Parent".to_owned(),
                link: "lines".to_owned(),
                source: "parent-1".to_owned(),
                target: "line-1".to_owned(),
                values: BTreeMap::new(),
                target_type: Some("defs.Line".to_owned()),
            }],
            ..CommitBatch::default()
        })
        .expect("seed live owned child and parent link");

    let before = store.read().verify_ledger().expect("ledger before check");
    assert!(matches!(
        store.check_definitions(
            &candidate,
            &install_input("owned-rows-remove-v2", 3, &candidate),
            Some(1)
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("owned-rows-remove-v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active definitions")
            .map(|installed| installed.version),
        Some(1)
    );
}

#[test]
fn removing_owned_collection_refuses_a_live_manifest_even_without_rows() {
    let previous = parent_rows_migration_model(true);
    let candidate = parent_rows_migration_model(false);
    let mut store = CellStore::open_in_memory(previous.clone()).expect("store opens");
    store
        .install_definitions(
            previous.clone(),
            install_input("owned-rows-manifest-remove-v1", 1, &previous),
            Some(0),
            "org_demo/dev",
        )
        .expect("owned collection installs");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![
                FactWriteRow::Put {
                    object_type: "defs.Parent".to_owned(),
                    key: "parent-1".to_owned(),
                    values: BTreeMap::from([(
                        "title".to_owned(),
                        Value::String("order".to_owned()),
                    )]),
                },
                FactWriteRow::Put {
                    object_type: OWNED_ROWS_TYPE.to_owned(),
                    key: "owned-manifest-1".to_owned(),
                    values: BTreeMap::from([
                        (
                            "parentType".to_owned(),
                            Value::String("defs.Parent".to_owned()),
                        ),
                        ("parentKey".to_owned(), Value::String("parent-1".to_owned())),
                        ("collectionId".to_owned(), Value::String("lines".to_owned())),
                        (
                            "rowIds".to_owned(),
                            Value::from_json(json!([])).expect("empty manifest row ids"),
                        ),
                    ]),
                },
            ],
            ..CommitBatch::default()
        })
        .expect("seed parent and empty manifest");

    let before = store.read().verify_ledger().expect("ledger before check");
    assert!(matches!(
        store.check_definitions(
            &candidate,
            &install_input("owned-rows-manifest-remove-v2", 3, &candidate),
            Some(1)
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );
}

#[test]
fn removing_an_inherited_lifecycle_state_with_live_objects_refuses() {
    let original = lifecycle_model(false);
    let candidate = lifecycle_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let values = BTreeMap::from([
        ("name".to_owned(), Value::String("Child".to_owned())),
        ("status".to_owned(), Value::String("active".to_owned())),
    ]);
    store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Child".to_owned(),
                key: "child-1".to_owned(),
                values,
            },
            2,
        ))
        .expect("create child in inherited state");

    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 2);
}

#[test]
fn tightened_enum_contract_accepts_compatible_values_and_refuses_live_violations() {
    let original = enum_model(&["open", "closed"]);
    let candidate = enum_model(&["open"]);

    let mut compatible_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    compatible_store
        .install_definitions(
            original.clone(),
            install_input("enum-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    compatible_store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Item".to_owned(),
                key: "item-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Open".to_owned())),
                    ("status".to_owned(), Value::String("open".to_owned())),
                ]),
            },
            2,
        ))
        .expect("create compatible row");
    compatible_store
        .install_definitions(
            candidate.clone(),
            install_input("enum-v2-compatible", 3, &candidate),
            Some(1),
            "org_demo/dev",
        )
        .expect("narrowing with compatible live value installs");
    assert_eq!(
        compatible_store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(2)
    );

    let mut violating_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    violating_store
        .install_definitions(
            original.clone(),
            install_input("enum-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    violating_store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Item".to_owned(),
                key: "item-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Closed".to_owned())),
                    ("status".to_owned(), Value::String("closed".to_owned())),
                ]),
            },
            2,
        ))
        .expect("create violating row");
    assert!(matches!(
        violating_store.install_definitions(
            candidate.clone(),
            install_input("enum-v2-invalid", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        violating_store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(
        violating_store
            .read()
            .verify_ledger()
            .expect("ledger")
            .commits,
        2
    );
}

#[test]
fn first_install_validates_all_legacy_live_values_with_a_bounded_candidate_scan() {
    let original = enum_model(&["a", "b"]);
    let candidate = enum_model(&["a"]);

    for (value, should_install) in [("a", true), ("b", false)] {
        let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
        store
            .commit(&batch(
                FactWriteRow::Put {
                    object_type: "defs.Item".to_owned(),
                    key: "legacy".to_owned(),
                    values: BTreeMap::from([
                        ("name".to_owned(), Value::String("Legacy".to_owned())),
                        ("status".to_owned(), Value::String(value.to_owned())),
                    ]),
                },
                1,
            ))
            .expect("legacy row is valid under the seed model");
        assert!(
            store
                .read()
                .installed_definitions(None)
                .expect("no installed set")
                .is_none()
        );
        let before = store.read().verify_ledger().expect("ledger before install");

        let result = store.install_definitions(
            candidate.clone(),
            install_input("legacy-candidate", 2, &candidate),
            Some(0),
            "org_demo/dev",
        );
        if should_install {
            assert_eq!(result.expect("compatible legacy value installs").version, 1);
        } else {
            assert!(matches!(
                result,
                Err(CellError::DefinitionMigrationRefused(_))
            ));
            assert!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("legacy history remains absent")
                    .is_none()
            );
            assert_eq!(
                store.read().verify_ledger().expect("ledger after refusal"),
                before
            );
            store
                .commit(&batch(
                    FactWriteRow::Put {
                        object_type: "defs.Item".to_owned(),
                        key: "still-usable".to_owned(),
                        values: BTreeMap::from([
                            ("name".to_owned(), Value::String("Still usable".to_owned())),
                            ("status".to_owned(), Value::String("b".to_owned())),
                        ]),
                    },
                    2,
                ))
                .expect("refusal leaves the legacy model active");
        }
    }
}

#[test]
fn definition_install_validates_candidate_invariants_over_live_values() {
    let original = total_invariant_model(true, None, false);
    let candidate = total_invariant_model(true, None, true);

    for (total, should_install) in [(-1, false), (7, true)] {
        let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
        store
            .install_definitions(
                original.clone(),
                install_input("invariant-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("invariant-free seed installs");
        store
            .commit(&batch(put_total("defs.Item", "item-1", total), 2))
            .expect("legacy value commits without an invariant");
        let before = store.read().verify_ledger().expect("ledger before install");

        let result = store.install_definitions(
            candidate.clone(),
            install_input("invariant-v2", 3, &candidate),
            Some(1),
            "org_demo/dev",
        );
        if should_install {
            assert_eq!(result.expect("valid object allows install").version, 2);
        } else {
            let Err(CellError::DefinitionInvariantViolation {
                violations,
                truncated,
            }) = result
            else {
                panic!("the negative live total must reject the candidate invariant")
            };
            assert!(!truncated);
            assert_eq!(violations.len(), 1);
            assert_eq!(violations[0].object.world, MAIN_WORLD);
            assert_eq!(violations[0].object.object_type, "defs.Item");
            assert_eq!(violations[0].object.id, "item-1");
            assert_eq!(violations[0].id.as_str(), "non_negative_total");
            assert_eq!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("active definitions remain readable")
                    .map(|installed| installed.version),
                Some(1)
            );
            assert_eq!(
                store.read().verify_ledger().expect("ledger after refusal"),
                before
            );
        }
    }
}

#[test]
fn quality_only_invariant_install_skips_scan_of_live_nonconforming_rows() {
    let original = model(false);
    let candidate = name_quality_invariant_model("false");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("quality-only-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..4_097)
        .map(|index| put_type("defs.Item", &format!("quality-{index}"), "legacy", None))
        .collect();
    store
        .commit(&rows)
        .expect("legacy rows commit before quality declaration");

    assert_eq!(
        store
            .install_definitions(
                candidate.clone(),
                install_input("quality-only-v2", 3, &candidate),
                Some(1),
                "org_demo/dev",
            )
            .expect("quality expectations do not scan/refuse nonconforming rows")
            .version,
        2
    );
    assert!(
        store
            .read()
            .object(MAIN_WORLD, "defs.Item", "quality-0")
            .expect("quality row reads")
            .is_some()
    );
}

#[test]
fn first_install_checks_new_default_values_before_activating_invariants() {
    let original = total_invariant_model(false, None, false);

    for (default, should_install) in [(-1, false), (1, true)] {
        let candidate = total_invariant_model(true, Some(default), true);
        let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
        store
            .commit(&batch(
                put_type("defs.Item", "defaulted", "Defaulted", None),
                1,
            ))
            .expect("legacy object commits without total");
        let before = store.read().verify_ledger().expect("ledger before install");

        let result = store.install_definitions(
            candidate.clone(),
            install_input("default-invariant", 2, &candidate),
            Some(0),
            "org_demo/dev",
        );
        if should_install {
            let installed = result.unwrap_or_else(|error| {
                panic!("non-negative default {default} should allow install; got {error:?}")
            });
            assert_eq!(installed.version, 1);
            assert_eq!(
                store
                    .read()
                    .object(MAIN_WORLD, "defs.Item", "defaulted")
                    .expect("object reads")
                    .expect("object remains live")
                    .values
                    .get("total")
                    .and_then(Value::as_i64),
                Some(default)
            );
        } else {
            assert!(
                matches!(&result, Err(CellError::DefinitionInvariantViolation { .. })),
                "negative candidate default should refuse with an invariant violation; got {result:?}"
            );
            assert!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("active definitions remain absent")
                    .is_none()
            );
            assert_eq!(
                store.read().verify_ledger().expect("ledger after refusal"),
                before
            );
            assert!(
                !store
                    .read()
                    .object(MAIN_WORLD, "defs.Item", "defaulted")
                    .expect("legacy object reads")
                    .expect("legacy object remains live")
                    .values
                    .contains_key("total"),
                "failed installation rolls back the new default column"
            );
            let clean_candidate = total_invariant_model(true, Some(1), true);
            assert_eq!(
                store
                    .install_definitions(
                        clean_candidate.clone(),
                        install_input("default-invariant-retry", 3, &clean_candidate),
                        Some(0),
                        "org_demo/dev",
                    )
                    .expect("failed DDL was rolled back and can be retried")
                    .version,
                1
            );
        }
    }
}

#[test]
fn inherited_candidate_invariant_checks_live_subtype_values() {
    let original = inherited_total_invariant_model(false);
    let candidate = inherited_total_invariant_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("inherited-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free seed installs");
    store
        .commit(&batch(put_total("defs.Child", "child-1", -4), 2))
        .expect("subtype value commits without invariant");

    let result = store.install_definitions(
        candidate.clone(),
        install_input("inherited-v2", 3, &candidate),
        Some(1),
        "org_demo/dev",
    );
    assert!(matches!(
        result,
        Err(CellError::DefinitionInvariantViolation { violations, .. })
            if violations.iter().any(|finding| finding.object.object_type == "defs.Child")
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active definitions remain readable")
            .map(|installed| installed.version),
        Some(1)
    );
}

#[test]
fn interface_invariant_scan_includes_objects_in_other_worlds() {
    let original = interface_total_invariant_model(false);
    let candidate = interface_total_invariant_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("interface-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free interface seed installs");
    let mut write = batch(put_total("defs.Item", "other-world-item", -1), 2);
    write.world = "simulation-17".to_owned();
    store
        .commit(&write)
        .expect("other-world value commits without invariant");

    let result = store.install_definitions(
        candidate.clone(),
        install_input("interface-v2", 3, &candidate),
        Some(1),
        "org_demo/dev",
    );
    assert!(matches!(
        result,
        Err(CellError::DefinitionInvariantViolation { violations, .. })
            if violations.iter().any(|finding|
                finding.object.world == "simulation-17"
                    && finding.owner == norn_ir::InvariantOwner::Interface(
                        norn_ir::InterfaceName("defs.NonnegativeTotal".to_owned())
                    )
            )
    ));
}

#[test]
fn installing_a_new_invariant_bearing_type_with_no_live_rows_succeeds() {
    let original = model_from_json(vec![json!({
        "name":"defs.Base",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"total", "type":{"kind":"integer"}}
        ]
    })]);
    let candidate = model_from_json(vec![
        json!({
            "name":"defs.Base",
            "properties":[
                {"id":"name", "type":{"kind":"text"}},
                {"id":"total", "type":{"kind":"integer"}}
            ]
        }),
        json!({
            "name":"defs.Child",
            "extends":"defs.Base",
            "invariants":[{
                "id":"non_negative_total",
                "expression":"self.total >= 0",
                "message":"total must be non-negative"
            }]
        }),
    ]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("new-type-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("base definitions install");
    store
        .commit(&batch(put_total("defs.Base", "base-1", -3), 2))
        .expect("base value has no candidate child invariant");

    assert_eq!(
        store
            .install_definitions(
                candidate.clone(),
                install_input("new-type-v2", 3, &candidate),
                Some(1),
                "org_demo/dev",
            )
            .expect("new concrete types have no existing rows to check")
            .version,
        2
    );
}

#[test]
fn unrelated_definition_install_skips_unchanged_invariants_on_known_bad_rows() {
    let original = total_invariant_model(true, None, true);
    let candidate = model_from_json(vec![
        serde_json::to_value(original.object_types().next().expect("item type"))
            .expect("item serializes"),
        serde_json::from_value(json!({
            "name":"defs.Other",
            "properties":[{"id":"name", "type":{"kind":"text"}}]
        }))
        .expect("other type decodes"),
    ]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("quality-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant definitions install");
    store
        .commit(&batch(put_total("defs.Item", "known-bad", -1), 2))
        .expect("the store records an existing quality defect");

    assert_eq!(
        store
            .install_definitions(
                candidate.clone(),
                install_input("unrelated-v2", 3, &candidate),
                Some(1),
                "org_demo/dev",
            )
            .expect("unchanged invariant contract does not revalidate unrelated rows")
            .version,
        2
    );
}

#[test]
fn candidate_invariant_scan_caps_findings_after_paging() {
    let original = total_invariant_model(true, None, false);
    let candidate = total_invariant_model(true, None, true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("many-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..35)
        .map(|index| put_total("defs.Item", &format!("negative-{index}"), -1))
        .collect();
    store.commit(&rows).expect("negative legacy rows commit");

    let result = store.install_definitions(
        candidate.clone(),
        install_input("many-v2", 3, &candidate),
        Some(1),
        "org_demo/dev",
    );
    assert!(matches!(
        result,
        Err(CellError::DefinitionInvariantViolation {
            violations,
            truncated: true
        }) if violations.len() == 32
    ));
}

#[test]
fn candidate_invariant_scan_refuses_more_than_4096_affected_rows() {
    let original = model(false);
    let candidate = name_invariant_model("true");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("row-budget-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..4_097)
        .map(|index| put_type("defs.Item", &format!("row-{index}"), "ok", None))
        .collect();
    store.commit(&rows).expect("legacy rows commit");
    let before = store.read().verify_ledger().expect("ledger before install");

    let result = store.install_definitions(
        candidate.clone(),
        install_input("row-budget-v2", 3, &candidate),
        Some(1),
        "org_demo/dev",
    );
    assert!(matches!(
        result,
        Err(CellError::DefinitionInvariantBudgetExceeded {
            resource: norn_cell::error::DefinitionInvariantBudget::Rows,
            limit: 4_096,
            scanned: 4_096,
            object: None
        })
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active definitions remain readable")
            .map(|installed| installed.version),
        Some(1)
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );
}

#[test]
fn candidate_invariant_scan_shares_fuel_across_live_rows() {
    let original = model(false);
    let candidate = name_invariant_model("len(self.name) >= 0");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("fuel-budget-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("invariant-free seed installs");
    let long_name = "n".repeat(10_000);
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..128)
        .map(|index| put_type("defs.Item", &format!("fuel-{index}"), &long_name, None))
        .collect();
    store.commit(&rows).expect("large legacy rows commit");
    let before = store.read().verify_ledger().expect("ledger before install");

    let result = store.install_definitions(
        candidate.clone(),
        install_input("fuel-budget-v2", 3, &candidate),
        Some(1),
        "org_demo/dev",
    );
    assert!(matches!(
        result,
        Err(CellError::DefinitionInvariantBudgetExceeded {
            resource: norn_cell::error::DefinitionInvariantBudget::Fuel,
            limit: 1_000_000,
            scanned,
            object: Some(_)
        }) if scanned <= 128
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active definitions remain readable")
            .map(|installed| installed.version),
        Some(1)
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );
}

#[test]
fn first_install_refuses_legacy_sources_missing_required_links() {
    let model = required_link_model();
    for (with_link, should_install) in [(false, false), (true, true)] {
        let mut store = CellStore::open_in_memory(model.clone()).expect("store opens");
        let mut legacy = batch(put_type("defs.Source", "source-1", "Source", None), 1);
        legacy.writes = vec![
            put_type("defs.Source", "source-1", "Source", None),
            put_type("defs.Target", "target-1", "Target", None),
        ];
        if with_link {
            legacy.links.push(LinkChange::Add {
                object_type: "defs.Source".to_owned(),
                link: "target".to_owned(),
                source: "source-1".to_owned(),
                target: "target-1".to_owned(),
                values: BTreeMap::new(),
                target_type: Some("defs.Target".to_owned()),
            });
        }
        store.commit(&legacy).expect("legacy objects are writable");

        let result = store.install_definitions(
            model.clone(),
            install_input("required-link-seed", 2, &model),
            Some(0),
            "org_demo/dev",
        );
        if should_install {
            assert_eq!(
                result
                    .expect("existing required link proves presence")
                    .version,
                1
            );
        } else {
            assert!(matches!(
                result,
                Err(CellError::DefinitionMigrationRefused(_))
            ));
            assert!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("history remains absent")
                    .is_none()
            );
            assert_eq!(store.read().verify_ledger().expect("ledger").commits, 1);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the endpoint regression covers narrowing, root identity, and empty-store behavior"
)]
fn narrowed_interface_target_checks_live_endpoint_root_and_membership() {
    let original = interface_link_model(true, true);
    let narrowed = interface_link_model(true, false);
    let mut compatible_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    compatible_store
        .install_definitions(
            original.clone(),
            install_input("party-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("party seed installs");
    let mut compatible_batch = batch(put_type("crm.Customer", "same", "Customer", None), 2);
    compatible_batch.writes = vec![
        put_type("crm.Customer", "same", "Customer", None),
        put_type("crm.Other", "same", "Other", None),
        put_type("sales.Order", "order-1", "Order", None),
    ];
    compatible_batch.links = vec![LinkChange::Add {
        object_type: "sales.Order".to_owned(),
        link: "customer".to_owned(),
        source: "order-1".to_owned(),
        target: "same".to_owned(),
        values: BTreeMap::new(),
        target_type: Some("crm.Customer".to_owned()),
    }];
    compatible_store
        .commit(&compatible_batch)
        .expect("link to the retained implementation installs");
    compatible_store
        .install_definitions(
            narrowed.clone(),
            install_input("party-v2-compatible", 3, &narrowed),
            Some(1),
            "org_demo/dev",
        )
        .expect("link remains valid despite an unrelated root sharing its id");
    assert_eq!(
        compatible_store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(2)
    );

    let mut violating_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    violating_store
        .install_definitions(
            original.clone(),
            install_input("party-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("party seed installs");
    let mut violating_batch = batch(put_type("crm.Customer", "customer-1", "Customer", None), 2);
    violating_batch.writes = vec![
        put_type("crm.Customer", "customer-1", "Customer", None),
        put_type("sales.Order", "order-1", "Order", None),
    ];
    violating_batch.links = vec![LinkChange::Add {
        object_type: "sales.Order".to_owned(),
        link: "customer".to_owned(),
        source: "order-1".to_owned(),
        target: "customer-1".to_owned(),
        values: BTreeMap::new(),
        target_type: Some("crm.Customer".to_owned()),
    }];
    violating_store
        .commit(&violating_batch)
        .expect("create live interface link");
    let removed_target = interface_link_model(false, true);
    assert!(matches!(
        violating_store.install_definitions(
            removed_target.clone(),
            install_input("party-v2-invalid", 3, &removed_target),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        violating_store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(
        violating_store
            .read()
            .verify_ledger()
            .expect("ledger")
            .commits,
        2
    );

    let mut empty_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    empty_store
        .install_definitions(
            original.clone(),
            install_input("party-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("party seed installs");
    let empty_candidate = interface_link_model(false, false);
    empty_store
        .install_definitions(
            empty_candidate.clone(),
            install_input("party-v2-empty", 2, &empty_candidate),
            Some(1),
            "org_demo/dev",
        )
        .expect("empty target set installs when no links can be orphaned");
}

#[test]
fn tightened_enum_scan_covers_inherited_and_overflow_values() {
    let original = inherited_enum_model(&["open", "closed"]);
    let candidate = inherited_enum_model(&["open"]);
    let mut inherited_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    inherited_store
        .install_definitions(
            original.clone(),
            install_input("inherited-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    inherited_store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Child".to_owned(),
                key: "child-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Child".to_owned())),
                    ("status".to_owned(), Value::String("closed".to_owned())),
                ]),
            },
            2,
        ))
        .expect("create inherited row");
    assert!(matches!(
        inherited_store.install_definitions(
            candidate.clone(),
            install_input("inherited-v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));

    let overflow_original = overflow_enum_model(&["open", "closed"]);
    let overflow_candidate = overflow_enum_model(&["open"]);
    let overflow_column = overflow_original
        .schema()
        .object_table("defs.Item")
        .expect("item table")
        .columns
        .iter()
        .find(|column| column.property.as_str() == "status")
        .expect("status column");
    assert!(overflow_column.column.is_none());
    let mut overflow_store =
        CellStore::open_in_memory(overflow_original.clone()).expect("overflow store opens");
    overflow_store
        .install_definitions(
            overflow_original.clone(),
            install_input("overflow-enum-v1", 1, &overflow_original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    overflow_store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Item".to_owned(),
                key: "item-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Overflow".to_owned())),
                    ("status".to_owned(), Value::String("closed".to_owned())),
                ]),
            },
            2,
        ))
        .expect("create overflow row");
    assert!(matches!(
        overflow_store.install_definitions(
            overflow_candidate.clone(),
            install_input("overflow-enum-v2", 3, &overflow_candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
}

#[test]
fn tightened_link_property_contract_refuses_live_violations() {
    let original = link_enum_model(&["lead", "contributor"]);
    let candidate = link_enum_model(&["lead"]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("link-enum-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![
                put_type("defs.Item", "item-1", "One", None),
                put_type("defs.Item", "item-2", "Two", None),
            ],
            links: vec![LinkChange::Add {
                object_type: "defs.Item".to_owned(),
                link: "next".to_owned(),
                source: "item-1".to_owned(),
                target: "item-2".to_owned(),
                values: BTreeMap::from([(
                    "role".to_owned(),
                    Value::String("contributor".to_owned()),
                )]),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("create live link");
    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("link-enum-v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
}

#[test]
fn decimal_unit_change_refuses_to_reinterpret_a_live_value() {
    let original = decimal_unit_model("kg");
    let candidate = decimal_unit_model("g");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("unit-kg-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Item".to_owned(),
                key: "item-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Weight".to_owned())),
                    (
                        "amount".to_owned(),
                        Value::from_json(json!(100)).expect("decimal value is exact"),
                    ),
                ]),
            },
            2,
        ))
        .expect("create live decimal");
    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("unit-g-v2", 3, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
}

#[test]
fn declared_defaults_backfill_existing_scalar_and_overflow_properties() {
    let original = model(false);
    let scalar_with_default = model_from_json(vec![item_type(vec![
        json!({"id":"name", "type":{"kind":"text"}}),
        json!({
            "id":"tag", "type":{"kind":"text"},
            "default":"backfilled"
        }),
    ])]);
    let mut scalar_store = CellStore::open_in_memory(original.clone()).expect("store opens");
    scalar_store
        .install_definitions(
            original.clone(),
            install_input("scalar-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    scalar_store
        .commit(&batch(put("Alpha", None), 2))
        .expect("create before scalar property");
    scalar_store
        .install_definitions(
            scalar_with_default.clone(),
            install_input("scalar-v2", 3, &scalar_with_default),
            Some(1),
            "org_demo/dev",
        )
        .expect("add scalar property with default");
    let scalar = scalar_store
        .read()
        .object(MAIN_WORLD, "defs.Item", "item-1")
        .expect("read")
        .expect("item remains");
    assert_eq!(
        scalar.values.get("tag").and_then(Value::as_str),
        Some("backfilled")
    );

    let overflow_original = overflow_model(false);
    let overflow_candidate = overflow_model(true);
    let mut overflow_store =
        CellStore::open_in_memory(overflow_original.clone()).expect("overflow store opens");
    overflow_store
        .install_definitions(
            overflow_original.clone(),
            install_input("overflow-v1", 1, &overflow_original),
            Some(0),
            "org_demo/dev",
        )
        .expect("overflow seed installs");
    overflow_store
        .commit(&batch(put("Overflow", None), 2))
        .expect("create before overflow property");
    overflow_store
        .install_definitions(
            overflow_candidate.clone(),
            install_input("overflow-v2", 3, &overflow_candidate),
            Some(1),
            "org_demo/dev",
        )
        .expect("add overflow boolean default");
    let overflow = overflow_store
        .read()
        .object(MAIN_WORLD, "defs.Item", "item-1")
        .expect("read")
        .expect("item remains");
    assert_eq!(overflow.values.get("enabled"), Some(&Value::Bool(true)));
    overflow_store
        .commit(&batch(
            FactWriteRow::Put {
                object_type: "defs.Item".to_owned(),
                key: "item-1".to_owned(),
                values: BTreeMap::from([
                    ("name".to_owned(), Value::String("Overflow".to_owned())),
                    ("enabled".to_owned(), Value::Bool(false)),
                ]),
            },
            4,
        ))
        .expect("write the migrated overflow boolean");
    let overflow = overflow_store
        .read()
        .object(MAIN_WORLD, "defs.Item", "item-1")
        .expect("read updated object")
        .expect("item remains");
    assert_eq!(overflow.values.get("enabled"), Some(&Value::Bool(false)));
}

#[test]
fn failed_migration_rolls_back_new_table_pointer_receipt_and_ddl() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("cell.sqlite");
    let original = model(false);
    let candidate = model_from_json(vec![
        item_type(vec![json!({"id":"name", "type":{"kind":"integer"}})]),
        json!({"name":"defs.Added"}),
    ]);
    let added_table = candidate
        .schema()
        .object_table("defs.Added")
        .expect("new type table")
        .table
        .clone();
    let mut store = CellStore::from_engine(
        NativeEngine::open(&path, StoreOptions::default()).expect("engine opens"),
        original.clone(),
    )
    .expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");

    assert!(matches!(
        store.install_definitions(
            candidate.clone(),
            install_input("v2", 2, &candidate),
            Some(1),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active set")
            .map(|set| set.version),
        Some(1)
    );
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 1);
    let observer = NativeEngine::open(&path, StoreOptions::default()).expect("observer opens");
    let table_exists = observer
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            &[SqlValue::from(added_table)],
            |row| row.get::<i64>(0),
        )
        .expect("query schema")
        .is_some();
    assert!(
        !table_exists,
        "the failed install rolled back its new table"
    );
}

#[test]
fn tampered_definition_history_fails_closed_on_reopen() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("cell.sqlite");
    let original = model(false);
    let mut store = CellStore::from_engine(
        NativeEngine::open(&path, StoreOptions::default()).expect("engine opens"),
        original.clone(),
    )
    .expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    drop(store);

    let tamper = NativeEngine::open(&path, StoreOptions::default()).expect("tamper engine opens");
    tamper
        .execute(
            "UPDATE \"_norn_definition_version\" SET \"canonical_set\" = ?1 WHERE \"version\" = 1",
            &[SqlValue::from("[]")],
        )
        .expect("tamper retained artifact bytes");
    drop(tamper);

    assert!(matches!(
        CellStore::from_engine(
            NativeEngine::open(&path, StoreOptions::default()).expect("reopen engine"),
            original,
        ),
        Err(CellError::LedgerCorrupt { .. })
    ));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one atomic readmission transition verifies refusal, retained evidence and ledger together"
)]
fn readmission_is_atomic_preserves_the_definition_set_and_ledgers_receipt_evidence() {
    let original = model(false);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    let (old_receipt, old_hash) = admission_receipt("0.1.0", "norn-zen/4");
    let mut first_input = install_input("v1", 1, &original);
    first_input.receipt_sha256 = old_hash.clone();
    first_input.admission_receipt = Some(old_receipt.clone());
    let first = store
        .install_definitions(original.clone(), first_input, Some(0), "org_demo/dev")
        .expect("initial definitions install");

    let (new_receipt, new_hash) = admission_receipt("0.1.0", "norn-zen/6");
    let readmission = DefinitionReadmission {
        previous_receipt_sha256: old_hash.clone(),
        previous_checker_version: "0.1.0".to_owned(),
        previous_expr_profile: "norn-zen/4".to_owned(),
        checker_version: "0.1.0".to_owned(),
        expr_profile: "norn-zen/6".to_owned(),
        layout_migration: false,
    };
    let before = store.read().verify_ledger().expect("ledger before");
    let mut invalid = install_input("readmit-invalid", 2, &original);
    invalid.receipt_sha256 = new_hash.clone();
    invalid.admission_receipt = Some(new_receipt.clone());
    invalid.readmission = Some(DefinitionReadmission {
        previous_receipt_sha256: "f".repeat(64),
        ..readmission.clone()
    });
    assert!(matches!(
        store.install_definitions(
            original.clone(),
            invalid,
            Some(first.version),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionReceiptInvalid(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger unchanged"),
        before
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active unchanged"),
        Some(first.clone())
    );

    let mut tampered = install_input("readmit-tampered", 2, &original);
    tampered.receipt_sha256 = new_hash.clone();
    tampered.admission_receipt = Some(new_receipt.replace("norn-zen/6", "norn-zen/4"));
    tampered.readmission = Some(readmission.clone());
    assert!(matches!(
        store.install_definitions(
            original.clone(),
            tampered,
            Some(first.version),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionReceiptInvalid(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("ledger unchanged"),
        before
    );

    let mut input = install_input("readmit", 2, &original);
    input.receipt_sha256 = new_hash.clone();
    input.admission_receipt = Some(new_receipt.clone());
    input.readmission = Some(readmission.clone());
    let upgraded = store
        .install_definitions(original, input, Some(first.version), "org_demo/dev")
        .expect("readmission commits");
    assert_eq!(
        upgraded.version,
        first.version.checked_add(1).expect("next version")
    );
    assert_eq!(upgraded.canonical_set, first.canonical_set);
    assert_eq!(upgraded.registry, first.registry);
    assert_eq!(upgraded.model_hash, first.model_hash);
    assert_eq!(upgraded.ddl_hash, first.ddl_hash);
    assert_eq!(
        upgraded.admission_receipt.as_deref(),
        Some(new_receipt.as_str())
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(Some(first.version))
            .expect("prior version remains")
            .expect("prior definition set")
            .admission_receipt
            .as_deref(),
        Some(old_receipt.as_str())
    );
    let events = store.read().ledger(0).expect("ledger events");
    let event = events.last().expect("readmission event");
    assert_eq!(
        event.event.get("type").and_then(Value::as_str),
        Some("definitions.readmitted")
    );
    assert_eq!(
        event
            .event
            .get("previousReceiptSha256")
            .and_then(Value::as_str),
        Some(old_hash.as_str())
    );
    assert_eq!(
        event.event.get("receiptSha256").and_then(Value::as_str),
        Some(new_hash.as_str())
    );
    let journal = store
        .export_record(upgraded.commit_seq)
        .expect("readmission journal")
        .expect("readmission export record");
    let install = journal.definition_install.expect("typed readmission");
    assert_eq!(install.readmission, Some(readmission));
    assert_eq!(install.installed, upgraded);
}

#[derive(Clone, Default)]
struct WriteCounter {
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
    forbid: std::rc::Rc<std::cell::Cell<bool>>,
    outgoing_reads: std::rc::Rc<std::cell::Cell<usize>>,
}

struct CountedEngine {
    inner: NativeEngine,
    counter: WriteCounter,
}

impl CountedEngine {
    fn record_write(&self) {
        self.counter
            .attempts
            .set(self.counter.attempts.get().saturating_add(1));
        assert!(
            !self.counter.forbid.get(),
            "check attempted a SQL write or transaction"
        );
    }
}

impl SqlEngine for CountedEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.record_write();
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        if sql.starts_with("SELECT \"_target\" FROM ") {
            assert!(sql.ends_with("LIMIT ?3"), "unbounded outgoing query: {sql}");
            assert!(matches!(params.last(), Some(SqlValue::Integer(limit)) if *limit <= 1001));
            self.counter
                .outgoing_reads
                .set(self.counter.outgoing_reads.get().saturating_add(1));
            let rows = self.inner.query(sql, params)?;
            assert!(
                rows.len() <= 1001,
                "outgoing read exceeded the sentinel limit"
            );
            return Ok(rows);
        }
        self.inner.query(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.record_write();
        self.inner.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.record_write();
        self.inner.transaction(f)
    }
}

type QueryPlan = (String, Vec<String>);

#[derive(Clone, Default)]
struct GroupedQueryPlan(std::rc::Rc<std::cell::RefCell<Option<QueryPlan>>>);

struct ExplainEngine {
    inner: NativeEngine,
    grouped_query: GroupedQueryPlan,
}

impl SqlEngine for ExplainEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        if sql.contains(" GROUP BY ") {
            let plan = self
                .inner
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)?
                .into_iter()
                .map(|values| norn_cell::engine::SqlRow(values).get::<String>(3))
                .collect::<Result<Vec<_>, _>>()?;
            *self.grouped_query.0.borrow_mut() = Some((sql.to_owned(), plan));
        }
        self.inner.query(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.inner.transaction(f)
    }
}

#[test]
fn definition_check_is_read_only_and_matches_install_preview_and_refusal() {
    let original = total_invariant_model(false, None, false);
    let counter = WriteCounter::default();
    let engine = CountedEngine {
        inner: NativeEngine::open_in_memory().expect("engine opens"),
        counter: counter.clone(),
    };
    let mut store = CellStore::from_engine(engine, original.clone()).expect("store opens");
    let initial = install_input("check-initial", 1, &original);
    counter.attempts.set(0);
    counter.forbid.set(true);
    let preview = store
        .check_definitions(&original, &initial, Some(0))
        .expect("initial candidate admitted");
    assert_eq!(counter.attempts.get(), 0);
    assert!(
        store
            .read()
            .installed_definitions(None)
            .expect("active read")
            .is_none()
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger"),
        norn_cell::read::LedgerHead::genesis()
    );

    counter.forbid.set(false);
    let installed = store
        .install_definitions(original, initial, Some(0), "org_demo/dev")
        .expect("initial install");
    assert_eq!(preview, installed);
    store
        .commit(&batch(put_type("defs.Item", "item-1", "Present", None), 2))
        .expect("live object commits");
    let before = store.read().verify_ledger().expect("ledger before check");
    let active = store
        .read()
        .installed_definitions(None)
        .expect("active before");
    let candidate = total_invariant_model(true, Some(-1), true);
    let input = install_input("check-invalid-default", 3, &candidate);
    counter.attempts.set(0);
    counter.forbid.set(true);
    let checked = store.check_definitions(&candidate, &input, Some(installed.version));
    assert!(
        matches!(
            &checked,
            Err(CellError::DefinitionInvariantViolation { .. })
        ),
        "{checked:?}"
    );
    assert_eq!(counter.attempts.get(), 0);
    assert_eq!(
        store.read().verify_ledger().expect("ledger after check"),
        before
    );
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("active after"),
        active
    );
    let stale = store.check_definitions(&candidate, &input, Some(0));
    assert!(
        matches!(&stale, Err(CellError::DefinitionVersionConflict { .. })),
        "{stale:?}"
    );
    assert_eq!(counter.attempts.get(), 0);
    counter.forbid.set(false);
    let refused =
        store.install_definitions(candidate, input, Some(installed.version), "org_demo/dev");
    assert!(
        matches!(
            &refused,
            Err(CellError::DefinitionInvariantViolation { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after refusal"),
        before
    );

    let accepted = total_invariant_model(true, Some(1), true);
    let input = install_input("check-valid-default", 4, &accepted);
    counter.attempts.set(0);
    counter.forbid.set(true);
    let preview = store
        .check_definitions(&accepted, &input, Some(1))
        .expect("valid default admitted");
    assert_eq!(counter.attempts.get(), 0);
    counter.forbid.set(false);
    let installed = store
        .install_definitions(accepted, input, Some(1), "org_demo/dev")
        .expect("valid default installs");
    assert_eq!(preview, installed);
}

#[test]
fn definition_check_matches_overflow_default_and_orphan_refusal() {
    let original = overflow_model(false);
    let candidate = overflow_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("overflow-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed install");
    store
        .commit(&batch(put("Existing", None), 2))
        .expect("live object");
    let input = install_input("overflow-v2", 3, &candidate);
    let before = store.read().verify_ledger().expect("ledger before");
    let preview = store
        .check_definitions(&candidate, &input, Some(1))
        .expect("overflow check");
    assert_eq!(
        store.read().verify_ledger().expect("ledger after check"),
        before
    );
    let actual = store
        .install_definitions(candidate, input, Some(1), "org_demo/dev")
        .expect("overflow install");
    assert_eq!(preview, actual);

    let removed = Model::from_definitions(&[], &[]).expect("empty model projects");
    let input = install_input("remove-live-type", 4, &removed);
    let checked = store.check_definitions(&removed, &input, Some(2));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(_))),
        "{checked:?}"
    );
    let refused = store.install_definitions(removed, input, Some(2), "org_demo/dev");
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(_))),
        "{refused:?}"
    );
}

#[test]
fn definition_check_refuses_a_new_unique_key_with_duplicate_live_values() {
    let original = model(false);
    for candidate in [
        model_from_json(vec![json!({
            "name":"defs.Item",
            "properties":[{"id":"name", "type":{"kind":"text"}}],
            "identity":[{"name":"by_name", "properties":["name"]}]
        })]),
        model_from_json(vec![json!({
            "name":"defs.Item",
            "properties":[
                {"id":"name", "type":{"kind":"text"}},
                {"id":"rank", "type":{"kind":"integer"}, "default":"5"}
            ],
            "identity":[{"name":"by_rank", "properties":["rank"]}]
        })]),
    ] {
        let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
        store
            .install_definitions(
                original.clone(),
                install_input("unique-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("seed installs");
        store
            .commit(&batch(put_type("defs.Item", "one", "same", None), 2))
            .expect("first object");
        store
            .commit(&batch(put_type("defs.Item", "two", "same", None), 3))
            .expect("second object");
        let input = install_input("unique-v2", 4, &candidate);
        let checked = store.check_definitions(&candidate, &input, Some(1));
        assert!(
            matches!(&checked, Err(CellError::DefinitionMigrationRefused(_))),
            "{checked:?}"
        );
        let refused = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
        assert!(
            matches!(&refused, Err(CellError::DefinitionMigrationRefused(_))),
            "{refused:?}"
        );
    }
}

#[test]
fn added_identity_index_uses_a_rowid_bounded_query_without_sorting_before_limit() {
    let original = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"rowid", "type":{"kind":"text"}, "constraints":{"nullable":true}}
        ]
    })]);
    let candidate = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"rowid", "type":{"kind":"text"}, "constraints":{"nullable":true}}
        ],
        "identity":[{"name":"by_name", "properties":["name"]}]
    })]);
    let grouped_query = GroupedQueryPlan::default();
    let engine = ExplainEngine {
        inner: NativeEngine::open_in_memory().expect("engine opens"),
        grouped_query: grouped_query.clone(),
    };
    let mut store = CellStore::from_engine(engine, original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("identity-plan-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");

    store
        .check_definitions(
            &candidate,
            &install_input("identity-plan-v2", 3, &candidate),
            Some(1),
        )
        .expect("empty identity scan passes");
    let (sql, plan) = grouped_query
        .0
        .borrow()
        .clone()
        .expect("identity grouping query was captured");
    assert!(
        sql.contains("SELECT \"_rowid_\" AS _migration_rowid")
            && sql.contains("ORDER BY \"_rowid_\" LIMIT ?1"),
        "identity preflight did not select the unshadowed rowid alias: {sql}"
    );
    assert!(
        !plan
            .iter()
            .any(|detail| detail.contains("USE TEMP B-TREE FOR ORDER BY")),
        "bounded identity query sorted before its limit: {plan:?}"
    );
}

#[test]
fn overflow_backfill_check_and_install_handle_a_new_rowid_property() {
    let original = rowid_overflow_model(false);
    let candidate = rowid_overflow_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("rowid-overflow-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let write = FactWriteRow::Put {
        object_type: "defs.Item".to_owned(),
        key: "rowid-overflow-item".to_owned(),
        values: BTreeMap::from([("name".to_owned(), Value::String("rowid-shadow".to_owned()))]),
    };
    store.commit(&batch(write, 2)).expect("live row commits");

    let input = install_input("rowid-overflow-v2", 3, &candidate);
    let preview = store
        .check_definitions(&candidate, &input, Some(1))
        .expect("check-only overflow backfill passes");
    let installed = store
        .install_definitions(candidate, input, Some(1), "org_demo/dev")
        .expect("overflow backfill installs");
    assert_eq!(preview, installed);
    let object = store
        .read()
        .object(MAIN_WORLD, "defs.Item", "rowid-overflow-item")
        .expect("object reads")
        .expect("object remains stored");
    assert_eq!(
        object.values.get("rowid"),
        Some(&Value::String("property-rowid".to_owned()))
    );
    assert_eq!(object.values.get("enabled"), Some(&Value::Bool(true)));
    assert_eq!(object.values.get("extra"), Some(&Value::Null));
}

#[test]
fn identity_preflight_accepts_exactly_the_row_limit() {
    let original = model(false);
    let candidate = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "identity":[{"name":"by_name", "properties":["name"]}]
    })]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("identity-row-boundary-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..4_096)
        .map(|index| {
            let value = format!("boundary-{index}");
            put_type("defs.Item", &value, &value, None)
        })
        .collect();
    store.commit(&rows).expect("row-limit fixture commits");

    let input = install_input("identity-row-boundary-v2", 3, &candidate);
    let preview = store
        .check_definitions(&candidate, &input, Some(1))
        .expect("4,096 rows with an admissible unique key are admitted");
    let installed = store
        .install_definitions(candidate, input, Some(1), "org_demo/dev")
        .expect("candidate installs at exactly the row limit");
    assert_eq!(preview, installed);
}

#[test]
fn identity_preflight_refuses_more_than_the_row_limit_in_check_and_install() {
    let original = model(false);
    let candidate = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "identity":[{"name":"by_name", "properties":["name"]}]
    })]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("identity-row-limit-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..4_097)
        .map(|index| {
            let value = format!("unique-{index}");
            put_type("defs.Item", &format!("row-{index}"), &value, None)
        })
        .collect();
    store.commit(&rows).expect("live rows commit");
    let before = store.read().dump().expect("snapshot before check");

    let input = install_input("identity-row-limit-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{checked:?}"
    );
    assert_eq!(store.read().dump().expect("snapshot after check"), before);
    let refused = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{refused:?}"
    );
    assert_eq!(store.read().dump().expect("snapshot after install"), before);
}

#[test]
fn identity_preflight_refuses_oversized_key_bytes_in_check_and_install() {
    let original = model(false);
    let candidate = model_from_json(vec![json!({
        "name":"defs.Item",
        "properties":[{"id":"name", "type":{"kind":"text"}}],
        "identity":[{"name":"by_name", "properties":["name"]}]
    })]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("identity-byte-limit-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let large = "x".repeat(8 * 1_024 * 1_024 + 1);
    store
        .commit(&batch(put_type("defs.Item", "item-1", &large, None), 2))
        .expect("large key value commits");
    let before_head = store.read().verify_ledger().expect("ledger before check");

    let input = install_input("identity-byte-limit-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{checked:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after check"),
        before_head
    );
    let refused = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{refused:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after install"),
        before_head
    );
    let object = store
        .read()
        .object(MAIN_WORLD, "defs.Item", "item-1")
        .expect("large key remains readable")
        .expect("large key remains stored");
    assert_eq!(
        object
            .values
            .get("name")
            .and_then(Value::as_str)
            .map(str::len),
        Some(large.len())
    );
}

#[test]
fn overflow_backfill_refuses_more_than_the_row_limit_in_check_and_install() {
    let original = overflow_model(false);
    let candidate = overflow_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("overflow-row-limit-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let mut rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        ..CommitBatch::default()
    };
    rows.writes = (0..4_097)
        .map(|index| put_type("defs.Item", &format!("overflow-{index}"), "same", None))
        .collect();
    store.commit(&rows).expect("live rows commit");
    let before = store.read().dump().expect("snapshot before check");

    let input = install_input("overflow-row-limit-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{checked:?}"
    );
    assert_eq!(store.read().dump().expect("snapshot after check"), before);
    let refused = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{refused:?}"
    );
    assert_eq!(store.read().dump().expect("snapshot after install"), before);
}

#[test]
fn overflow_backfill_charges_multibyte_payload_bytes_before_fetching() {
    let original = overflow_model(false);
    let candidate = overflow_model(true);
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("cell.sqlite");
    let mut store = CellStore::from_engine(
        NativeEngine::open(&path, StoreOptions::default()).expect("store engine opens"),
        original.clone(),
    )
    .expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("overflow-byte-limit-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let large = "é".repeat((8 * 1_024 * 1_024) / 2 + 1);
    let row_ids = vec!["large-overflow".to_owned()];
    let engine = seed_migration_budget_rows(MigrationBudgetRows {
        path: &path,
        model: &original,
        store: &mut store,
        world: "simulation-17",
        row_ids: &row_ids,
        value_column: OVERFLOW_COLUMN,
        value: serde_json::to_string(&json!({"field_109":large}))
            .expect("overflow fixture JSON serializes"),
        distinct_names: false,
    });
    let before_bytes =
        migration_budget_column_bytes(&engine, &original, OVERFLOW_COLUMN, "simulation-17");
    assert!(before_bytes.0 > 8 * 1_024 * 1_024);
    assert_eq!(before_bytes.0, before_bytes.1);
    let before_head = store.read().verify_ledger().expect("ledger before check");
    let before_versions = migration_budget_version_snapshot(&engine, "simulation-17");
    let before_version = store
        .read()
        .installed_definitions(None)
        .expect("active definitions before check")
        .expect("active definition set")
        .version;

    let input = install_input("overflow-byte-limit-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(before_version));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{checked:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after check"),
        before_head
    );
    let refused = store.install_definitions(candidate, input, Some(before_version), "org_demo/dev");
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{refused:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger after install"),
        before_head
    );
    assert_eq!(
        migration_budget_column_bytes(&engine, &original, OVERFLOW_COLUMN, "simulation-17"),
        before_bytes
    );
    assert_eq!(
        migration_budget_version_snapshot(&engine, "simulation-17"),
        before_versions
    );
    let active = store
        .read()
        .installed_definitions(None)
        .expect("active definitions after refusal")
        .expect("active definition set");
    assert_eq!(active.version, before_version);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each scan path needs an oversized aggregate fixture and unchanged-state assertions"
)]
fn migration_byte_budgets_cover_multiple_rows_below_the_do_row_limit() {
    for scenario in ["identity", "backfill", "removal"] {
        let overflow = scenario != "identity";
        let original = if overflow {
            overflow_model(false)
        } else {
            model(false)
        };
        let candidate = if scenario == "removal" {
            let mut definitions = original.object_types().cloned().collect::<Vec<_>>();
            for definition in &mut definitions {
                definition
                    .properties
                    .retain(|property| property.id.as_str() != "field_108");
            }
            Model::from_definitions(&definitions, &[]).expect("removal candidate projects")
        } else if overflow {
            overflow_model(true)
        } else {
            model_from_json(vec![json!({
                "name":"defs.Item",
                "properties":[{"id":"name", "type":{"kind":"text"}}],
                "identity":[{"name":"by_name", "properties":["name"]}]
            })])
        };
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("cell.sqlite");
        let mut store = CellStore::from_engine(
            NativeEngine::open(&path, StoreOptions::default()).expect("store engine opens"),
            original.clone(),
        )
        .expect("store opens");
        store
            .install_definitions(
                original.clone(),
                install_input("aggregate-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("seed installs");
        let payload = "é".repeat(900_000);
        let row_ids = (0..5)
            .map(|index| format!("row-{index}"))
            .collect::<Vec<_>>();
        let (value_column, value, distinct_names) = if overflow {
            (
                OVERFLOW_COLUMN,
                serde_json::to_string(&json!({"field_109":payload}))
                    .expect("overflow fixture JSON serializes"),
                false,
            )
        } else {
            ("name", payload, true)
        };
        let engine = seed_migration_budget_rows(MigrationBudgetRows {
            path: &path,
            model: &original,
            store: &mut store,
            world: MAIN_WORLD,
            row_ids: &row_ids,
            value_column,
            value,
            distinct_names,
        });
        let before_bytes =
            migration_budget_column_bytes(&engine, &original, value_column, MAIN_WORLD);
        assert!(before_bytes.0 > 8 * 1_024 * 1_024);
        assert!(before_bytes.1 < 2 * 1_024 * 1_024);
        let before = store.read().verify_ledger().expect("ledger before proof");
        let before_versions = migration_budget_version_snapshot(&engine, MAIN_WORLD);
        let before_version = store
            .read()
            .installed_definitions(None)
            .expect("definitions before proof")
            .expect("active definitions before proof")
            .version;
        let input = install_input("aggregate-v2", 10, &candidate);
        let checked = store.check_definitions(&candidate, &input, Some(1));
        assert!(
            matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
            "{checked:?}"
        );
        let installed = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
        assert!(
            matches!(&installed, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
            "{installed:?}"
        );
        assert_eq!(
            store.read().verify_ledger().expect("ledger after refusals"),
            before
        );
        assert_eq!(
            migration_budget_column_bytes(&engine, &original, value_column, MAIN_WORLD),
            before_bytes
        );
        assert_eq!(
            migration_budget_version_snapshot(&engine, MAIN_WORLD),
            before_versions
        );
        assert_eq!(
            store
                .read()
                .installed_definitions(None)
                .expect("definitions")
                .expect("active")
                .version,
            before_version
        );
    }
}

#[test]
fn removing_an_empty_overflow_property_refuses_an_incomplete_row_scan() {
    let original = overflow_model(false);
    let mut definitions = original.object_types().cloned().collect::<Vec<_>>();
    for definition in &mut definitions {
        definition
            .properties
            .retain(|property| property.id.as_str() != "field_109");
    }
    let candidate = Model::from_definitions(&definitions, &[]).expect("candidate projects");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("remove-overflow-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    let rows = CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 2,
        writes: (0..4_097)
            .map(|index| put_type("defs.Item", &format!("row-{index}"), "same", None))
            .collect(),
        ..CommitBatch::default()
    };
    store.commit(&rows).expect("live rows commit");
    let before = store.read().dump().expect("before proof");
    let input = install_input("remove-overflow-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{checked:?}"
    );
    let installed = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&installed, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row migration scan limit")),
        "{installed:?}"
    );
    assert_eq!(store.read().dump().expect("after refusals"), before);
}

#[test]
fn first_overflow_column_charges_the_same_empty_object_bytes_in_check_and_install() {
    let mut properties = vec![json!({"id":"name", "type":{"kind":"text"}})];
    properties.extend((0..93).map(|index| {
        json!({
            "id":format!("field_{index}"),
            "type":{"kind":"text"},
            "constraints":{"nullable":true}
        })
    }));
    let original = model_from_json(vec![json!({"name":"defs.Item", "properties":properties})]);
    properties.push(json!({"id":"enabled", "type":{"kind":"boolean"}, "default":true}));
    properties.push(json!({"id":"extra", "type":{"kind":"text"}, "constraints":{"nullable":true}}));
    let candidate = model_from_json(vec![json!({"name":"defs.Item", "properties":properties})]);
    assert!(!original.schema().object_tables[0].has_overflow);
    assert!(candidate.schema().object_tables[0].has_overflow);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store opens");
    store
        .install_definitions(
            original.clone(),
            install_input("first-overflow-v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("seed installs");
    // Each row costs 64 overhead + 4 world + 1,952 id + 27 defaults + 2 '{}'
    // bytes: 2,049. Omitting '{}' would incorrectly admit this 4,096-row scan.
    let suffix = "x".repeat(1_948);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: (0..4_096)
                .map(|index| put_type("defs.Item", &format!("{index:04}{suffix}"), "same", None))
                .collect(),
            ..CommitBatch::default()
        })
        .expect("boundary rows commit");
    let before = store.read().verify_ledger().expect("before proof");
    let input = install_input("first-overflow-v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{checked:?}"
    );
    let installed = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&installed, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("8 MiB migration scan byte limit")),
        "{installed:?}"
    );
    assert_eq!(
        store.read().verify_ledger().expect("after refusals"),
        before
    );
}

fn hierarchy_model(acyclic: bool) -> Model {
    model_from_json(vec![
        json!({"name":"defs.Node", "properties":[{"id":"name","type":{"kind":"text"}}],
            "links":[{"id":"parent","target":"defs.Node","acyclic":acyclic,"cardinality":{"to":"one","from":"many"}}]}),
        json!({"name":"defs.TreeNode", "extends":"defs.Node"}),
    ])
}

fn hierarchy_link(source: &str, target: &str) -> LinkChange {
    LinkChange::Add {
        object_type: "defs.TreeNode".to_owned(),
        link: "parent".to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        values: BTreeMap::new(),
        target_type: None,
    }
}

#[test]
fn newly_acyclic_inherited_role_proves_live_graph_in_check_and_install_across_worlds() {
    for cyclic in [false, true] {
        for world in [MAIN_WORLD, "world-branch"] {
            let original = hierarchy_model(false);
            let candidate = hierarchy_model(true);
            let mut store = CellStore::open_in_memory(original.clone()).expect("store");
            store
                .install_definitions(
                    original.clone(),
                    install_input("v1", 1, &original),
                    Some(0),
                    "org_demo/dev",
                )
                .expect("initial");
            let mut links = vec![hierarchy_link("a", "b")];
            if cyclic {
                links.push(hierarchy_link("b", "a"));
            }
            store
                .commit(&CommitBatch {
                    world: world.to_owned(),
                    scope: "org_demo/dev".to_owned(),
                    logical_time: 2,
                    writes: vec![
                        put_type("defs.TreeNode", "a", "A", None),
                        put_type("defs.TreeNode", "b", "B", None),
                    ],
                    links,
                    ..CommitBatch::default()
                })
                .expect("unconstrained graph");
            let before = store.read().dump().expect("before");
            let input = install_input("v2", 3, &candidate);
            let checked = store.check_definitions(&candidate, &input, Some(1));
            assert_eq!(store.read().dump().expect("after check"), before);
            let installed = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
            if cyclic {
                assert!(
                    matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("cycle")),
                    "{checked:?}"
                );
                assert!(
                    matches!(&installed, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("cycle")),
                    "{installed:?}"
                );
                assert_eq!(store.read().dump().expect("after refusal"), before);
                assert_eq!(
                    store
                        .read()
                        .installed_definitions(None)
                        .expect("definitions")
                        .expect("active")
                        .version,
                    1
                );
                assert_eq!(store.read().verify_ledger().expect("ledger").commits, 2);
            } else {
                assert_eq!(
                    checked.expect("valid check"),
                    installed.expect("valid install")
                );
                assert_eq!(store.read().verify_ledger().expect("ledger").commits, 3);
            }
        }
    }
}

#[test]
fn newly_acyclic_role_refuses_incomplete_live_graph_proof() {
    let original = hierarchy_model(false);
    let candidate = hierarchy_model(true);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("initial");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: (0..=4097)
                .map(|i| put_type("defs.TreeNode", &i.to_string(), "Node", None))
                .collect(),
            links: (0..4097)
                .map(|i| hierarchy_link(&i.to_string(), &(i + 1).to_string()))
                .collect(),
            ..CommitBatch::default()
        })
        .expect("large unconstrained chain");
    let before = store.read().dump().expect("before");
    let input = install_input("v2", 3, &candidate);
    let checked = store.check_definitions(&candidate, &input, Some(1));
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row")),
        "{checked:?}"
    );
    let installed = store.install_definitions(candidate, input, Some(1), "org_demo/dev");
    assert!(
        matches!(&installed, Err(CellError::DefinitionMigrationRefused(message)) if message.contains("4,096-row")),
        "{installed:?}"
    );
    assert_eq!(store.read().dump().expect("after"), before);
}

#[test]
fn hierarchy_sql_host_bounds_outgoing_reads_before_materializing_high_fan_in() {
    use norn_kernel::{
        CellCommit, CellHost, Command, FactKey, FactVersion, LinkKey, LinkMutation, LinkWrite,
    };
    let model = hierarchy_model(true);
    let counter = WriteCounter::default();
    let engine = CountedEngine {
        inner: NativeEngine::open_in_memory().expect("engine"),
        counter: counter.clone(),
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            writes: (0..1100)
                .map(|i| put_type("defs.TreeNode", &format!("child-{i}"), "Child", None))
                .chain([
                    put_type("defs.TreeNode", "ancestor", "Ancestor", None),
                    put_type("defs.TreeNode", "middle", "Middle", None),
                    put_type("defs.TreeNode", "leaf", "Leaf", None),
                ])
                .collect(),
            links: (0..1100)
                .map(|i| hierarchy_link(&format!("child-{i}"), "ancestor"))
                .chain([hierarchy_link("middle", "ancestor")])
                .collect(),
            ..CommitBatch::default()
        })
        .expect("fan-in seed");
    let mut cell: norn_cell::SqliteCell<String, String, String, _> =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |command: &Command<String>| {
            command.payload.clone()
        })
        .expect("host");
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(
                FactKey::new("defs.TreeNode", "leaf"),
                "parent",
                FactKey::new("defs.TreeNode", "middle"),
            ),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: BTreeMap::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("bounded outgoing traversal ignores incoming fan-in");
    assert_eq!(
        counter.outgoing_reads.get(),
        2,
        "only middle and ancestor adjacency was read"
    );
}

#[test]
fn required_to_many_scalar_updates_use_bounded_presence_above_the_adjacency_limit() {
    use norn_kernel::{CellCommit, CellHost, Command, FactKey, FactMutation, FactWrite};
    let model = model_from_json(vec![
        json!({"name":"defs.Item", "properties":[{"id":"name","type":{"kind":"text"}}],
            "links":[{"id":"owners","target":"defs.Owner","cardinality":{"to":"many","from":"many","required":true}}]}),
        json!({"name":"defs.Owner"}),
    ]);
    let counter = WriteCounter::default();
    let engine = CountedEngine {
        inner: NativeEngine::open_in_memory().expect("engine"),
        counter: counter.clone(),
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 1,
            writes: std::iter::once(put_type("defs.Item", "item", "Item", None))
                .chain((0..1002).map(|i| FactWriteRow::Put {
                    object_type: "defs.Owner".to_owned(),
                    key: format!("owner-{i:04}"),
                    values: BTreeMap::new(),
                }))
                .collect(),
            links: (0..1001)
                .map(|i| LinkChange::Add {
                    object_type: "defs.Item".to_owned(),
                    link: "owners".to_owned(),
                    source: "item".to_owned(),
                    target: format!("owner-{i:04}"),
                    values: BTreeMap::new(),
                    target_type: None,
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("valid many-owner seed");
    let mut cell: norn_cell::SqliteCell<String, String, String, _> =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |command: &Command<String>| {
            command.payload.clone()
        })
        .expect("host");
    let key = FactKey::new("defs.Item", "item");
    let expected = cell.load(&key).expect("item").version;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected,
            mutation: FactMutation::Put {
                values: BTreeMap::from([("name".to_owned(), Value::String("Updated".to_owned()))]),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("presence needs one witness, not the full 1001-target adjacency");
    assert_eq!(counter.outgoing_reads.get(), 1);
}

#[test]
fn newly_acyclic_role_ignores_unrelated_large_same_name_graph() {
    let original = model_from_json(vec![
        json!({"name":"defs.A", "links":[{"id":"parent","target":"defs.A","cardinality":{"to":"one","from":"many"}}]}),
        json!({"name":"defs.B", "properties":[{"id":"name","type":{"kind":"text"}}], "links":[{"id":"parent","target":"defs.B","cardinality":{"to":"one","from":"many"}}]}),
    ]);
    let candidate = model_from_json(vec![
        json!({"name":"defs.A", "links":[{"id":"parent","target":"defs.A","acyclic":true,"cardinality":{"to":"one","from":"many"}}]}),
        json!({"name":"defs.B", "properties":[{"id":"name","type":{"kind":"text"}}], "links":[{"id":"parent","target":"defs.B","cardinality":{"to":"one","from":"many"}}]}),
    ]);
    let mut store = CellStore::open_in_memory(original.clone()).expect("store");
    store
        .install_definitions(
            original.clone(),
            install_input("v1", 1, &original),
            Some(0),
            "org_demo/dev",
        )
        .expect("initial");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: (0..=4097)
                .map(|i| put_type("defs.B", &i.to_string(), "B", None))
                .collect(),
            links: (0..4097)
                .map(|i| LinkChange::Add {
                    object_type: "defs.B".to_owned(),
                    link: "parent".to_owned(),
                    source: i.to_string(),
                    target: (i + 1).to_string(),
                    values: BTreeMap::new(),
                    target_type: None,
                })
                .collect(),
            ..CommitBatch::default()
        })
        .expect("unrelated valid graph");
    let before = store.read().dump().expect("before");
    let input = install_input("v2", 3, &candidate);
    let check = store
        .check_definitions(&candidate, &input, Some(1))
        .expect("A proof is empty");
    assert_eq!(store.read().dump().expect("after check"), before);
    assert_eq!(
        check,
        store
            .install_definitions(candidate, input, Some(1), "org_demo/dev")
            .expect("A admits")
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one native migration scenario proves live widening and atomic refusal of other ownership changes"
)]
fn live_owned_collection_accepts_only_monotonic_bound_increases() {
    let previous = parent_rows_migration_model(true);
    let mut store = CellStore::open_in_memory(previous.clone()).expect("store opens");
    store
        .install_definitions(
            previous.clone(),
            install_input("bounds-v1", 1, &previous),
            Some(0),
            "org_demo/dev",
        )
        .expect("initial install");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 2,
            writes: vec![
                FactWriteRow::Put {
                    object_type: "defs.Parent".to_owned(),
                    key: "parent-1".to_owned(),
                    values: BTreeMap::from([(
                        "title".to_owned(),
                        Value::String("order".to_owned()),
                    )]),
                },
                FactWriteRow::Put {
                    object_type: "defs.Line".to_owned(),
                    key: "line-1".to_owned(),
                    values: BTreeMap::from([
                        ("row_id".to_owned(), Value::String("line-1".to_owned())),
                        ("idx".to_owned(), integer_value(1)),
                        ("label".to_owned(), Value::String("item".to_owned())),
                    ]),
                },
            ],
            links: vec![LinkChange::Add {
                object_type: "defs.Parent".to_owned(),
                link: "lines".to_owned(),
                source: "parent-1".to_owned(),
                target: "line-1".to_owned(),
                values: BTreeMap::new(),
                target_type: Some("defs.Line".to_owned()),
            }],
            ..CommitBatch::default()
        })
        .expect("seed live rows and link");
    let mut snapshot = previous.definitions_snapshot();
    snapshot
        .object_types
        .iter_mut()
        .find(|object| object.name.as_str() == "defs.Parent")
        .expect("parent")
        .owned_collections[0]
        .max_rows = norn_ir::WholeNumber(20);
    let widened = Model::from_snapshot(&snapshot).expect("widened model");
    store
        .check_definitions(&widened, &install_input("bounds-v2", 3, &widened), Some(1))
        .expect("bound increase previews");
    store
        .install_definitions(
            widened.clone(),
            install_input("bounds-v2", 3, &widened),
            Some(1),
            "org_demo/dev",
        )
        .expect("bound increase installs over live data");
    assert!(
        store
            .read()
            .object(MAIN_WORLD, "defs.Line", "line-1")
            .expect("read")
            .is_some()
    );
    let before = store.read().verify_ledger().expect("ledger");
    let mut narrowed = snapshot.clone();
    narrowed
        .object_types
        .iter_mut()
        .find(|object| object.name.as_str() == "defs.Parent")
        .expect("parent")
        .owned_collections[0]
        .max_rows = norn_ir::WholeNumber(10);
    let narrowed = Model::from_snapshot(&narrowed).expect("narrowed model");
    assert!(matches!(
        store.install_definitions(
            narrowed.clone(),
            install_input("bounds-v3", 4, &narrowed),
            Some(2),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    let parent = snapshot
        .object_types
        .iter_mut()
        .find(|object| object.name.as_str() == "defs.Parent")
        .expect("parent");
    parent.owned_collections[0].cascade_on_parent_delete = false;
    let changed = Model::from_snapshot(&snapshot).expect("changed model");
    assert!(matches!(
        store.install_definitions(
            changed.clone(),
            install_input("bounds-v3-cascade", 4, &changed),
            Some(2),
            "org_demo/dev"
        ),
        Err(CellError::DefinitionMigrationRefused(_))
    ));
    assert_eq!(
        store.read().verify_ledger().expect("unchanged ledger"),
        before
    );
}

#[path = "quantity_support/mod.rs"]
mod quantity_v2_support;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture proves pinned retry identity through defaults, export and restore"
)]
async fn quantity_retry_binds_first_definition_and_submitted_values_before_new_defaults() {
    use norn_kernel::{CellCommit, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime};
    let original = model_from_json(vec![item_type(vec![
        json!({"id":"name","type":{"kind":"text"}}),
        json!({"id":"tag","type":{"kind":"text"},"default":"first-default"}),
    ])]);
    let mut cell: norn_cell::SqliteCell<String, String, String> = norn_cell::SqliteCell::new(
        CellStore::open_in_memory(original.clone()).expect("store"),
        "org_demo/dev",
        |_| "ok".into(),
    )
    .expect("host");
    let mut original_input = install_input("quantity-v1", 1, &original);
    // Export validation requires digest-shaped identities for these trusted fixtures.
    original_input.receipt_sha256 = "a".repeat(64);
    cell.install_definitions(original.clone(), original_input, Some(0))
        .expect("seed definitions");
    cell.advance_to(LogicalTime::from_millis(2));
    cell.commit_quantity(&quantity_v2_support::define(), "define")
        .expect("define quantity");
    let mut quantity = quantity_v2_support::post(0, "fund", "fund", 20);
    quantity.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &quantity)
        .expect("fences");
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key: FactKey::new("defs.Item", "quantity-item"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: BTreeMap::from([("name".into(), Value::String("original".into()))]),
        },
    });
    let first = cell
        .commit_with_quantity(commit.clone(), &quantity, "first")
        .expect("combined first commit");
    let pinned = cell
        .store()
        .read()
        .receipt_by_idempotency_key("main", "first")
        .expect("receipt")
        .expect("first");
    assert_eq!(pinned.body.definitions_version, 1);
    let mut held = quantity_v2_support::reserve(0, "later-hold", 5, 100);
    held.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &held)
        .expect("hold fences");
    cell.commit_quantity(&held, "later-hold")
        .expect("quantity state progresses before retry");

    let candidate = model_from_json(vec![item_type(vec![
        json!({"id":"name","type":{"kind":"text"}}),
        json!({"id":"tag","type":{"kind":"text"},"default":"later-default"}),
    ])]);
    let mut candidate_input = install_input("quantity-v2", 3, &candidate);
    candidate_input.receipt_sha256 = "b".repeat(64);
    cell.install_definitions(candidate.clone(), candidate_input, Some(1))
        .expect("new defaults");
    cell.advance_to(LogicalTime::from_millis(4));
    let before = cell.store().read().dump().expect("dump");
    assert_eq!(
        cell.commit_with_quantity(commit.clone(), &quantity, "first")
            .expect("old input replays before defaults"),
        first
    );
    assert_eq!(
        before,
        cell.store().read().dump().expect("retry has no mutation")
    );
    let mut changed_expectation = commit.clone();
    changed_expectation.writes[0].expected = FactVersion(1);
    assert!(matches!(
        cell.commit_with_quantity(changed_expectation, &quantity, "first"),
        Err(norn_kernel::CommitError::Host(
            CellError::IdempotencyConflict(_)
        ))
    ));
    let mut changed = commit.clone();
    if let FactMutation::Put { values } = &mut changed.writes[0].mutation {
        values.insert("tag".into(), Value::String("later-default".into()));
    }
    assert!(matches!(
        cell.commit_with_quantity(changed, &quantity, "first"),
        Err(norn_kernel::CommitError::Host(
            CellError::IdempotencyConflict(_)
        ))
    ));
    assert_eq!(
        pinned,
        cell.store()
            .read()
            .receipt_by_idempotency_key("main", "first")
            .expect("receipt")
            .expect("first receipt preserved")
    );
    let dir = tempfile::tempdir().expect("directory");
    let mut exporter =
        norn_cell::export::Exporter::local(&dir.path().join("export")).expect("exporter");
    let ledger_tip = cell.store().read().verify_ledger().expect("verified head");
    for seq in 1..=ledger_tip.commits {
        let record = cell
            .store()
            .export_record(seq)
            .expect("journal")
            .expect("record");
        exporter.append(&record).await.expect("export");
    }
    let restored_path = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &restored_path,
        candidate.clone(),
        "org_demo/dev",
        &BTreeMap::from([("scope".into(), "org_demo/dev".into())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore recomputes original fingerprint");
    let mut restored: norn_cell::SqliteCell<String, String, String> = norn_cell::SqliteCell::new(
        CellStore::open(&restored_path, candidate, StoreOptions::default())
            .expect("restored store"),
        "org_demo/dev",
        |_| "ok".into(),
    )
    .expect("restored host");
    assert_eq!(
        restored
            .commit_with_quantity(commit, &quantity, "first")
            .expect("restored retry"),
        first
    );
    assert_eq!(
        restored
            .store()
            .read()
            .receipt_by_idempotency_key("main", "first")
            .expect("receipt"),
        Some(pinned),
    );
}

#[test]
fn record_defaults_backfill_exact_numbers_in_columns_and_overflow() {
    for extra_fields in [0, 110] {
        let mut properties = vec![json!({"id":"name","type":{"kind":"text"}})];
        properties.extend((0..extra_fields).map(|index| {
            json!({
                "id":format!("field_{index}"),"type":{"kind":"text"},"constraints":{"nullable":true}
            })
        }));
        let original = model_from_json(vec![item_type(properties.clone())]);
        properties.push(json!({
            "id":"zz_record","type":{"kind":"record","members":[
                {"id":"amount","type":{"kind":"decimal","scale":2}}
            ]},"constraints":{"nullable":true},"default":{"amount":"1.25"}
        }));
        let candidate = model_from_json(vec![item_type(properties)]);
        let column = candidate
            .schema()
            .object_table("defs.Item")
            .expect("table")
            .columns
            .iter()
            .find(|column| column.property.as_str() == "zz_record")
            .expect("record column");
        assert_eq!(
            column.column.is_none(),
            extra_fields > 0,
            "exercise both storage encodings"
        );
        let mut store = CellStore::open_in_memory(original.clone()).expect("store");
        store
            .install_definitions(
                original.clone(),
                install_input("record-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("initial install");
        store
            .commit(&batch(put("Alpha", None), 2))
            .expect("create before migration");
        store
            .install_definitions(
                candidate.clone(),
                install_input("record-v2", 3, &candidate),
                Some(1),
                "org_demo/dev",
            )
            .expect("record default migration");
        let object = store
            .read()
            .object(MAIN_WORLD, "defs.Item", "item-1")
            .expect("read")
            .expect("object");
        let expected = Value::from_json_text(r#"{"amount":1.25}"#).expect("exact expected record");
        assert_eq!(object.values.get("zz_record"), Some(&expected));
    }
}
