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
        } else if distinct_names && column == "p:name" {
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

type BeforeTransaction = Box<dyn FnOnce() -> Result<(), CellError>>;

#[derive(Clone, Default)]
struct WriteCounter {
    before_transaction: std::rc::Rc<std::cell::RefCell<Option<BeforeTransaction>>>,
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
    forbid: std::rc::Rc<std::cell::Cell<bool>>,
    outgoing_reads: std::rc::Rc<std::cell::Cell<usize>>,
    evidence_reads: std::rc::Rc<std::cell::Cell<usize>>,
    revision_bumps: std::rc::Rc<std::cell::Cell<usize>>,
    revision_epoch: std::rc::Rc<std::cell::Cell<u64>>,
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
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        let Some((writes, remote)) = self.inner.read_cache_revision()? else {
            return Ok(None);
        };
        if self.counter.revision_bumps.get() > 0 {
            self.counter
                .revision_bumps
                .set(self.counter.revision_bumps.get().saturating_sub(1));
            let epoch = self
                .counter
                .revision_epoch
                .get()
                .checked_add(1)
                .ok_or(CellError::OutOfRange("test revision"))?;
            self.counter.revision_epoch.set(epoch);
        }
        let remote = remote
            .checked_add(self.counter.revision_epoch.get())
            .ok_or(CellError::OutOfRange("test revision"))?;
        Ok(Some((writes, remote)))
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.record_write();
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        if sql.contains("observation_id LIKE 'mapping-conflict:%'")
            || sql.contains("observation_id LIKE 'mapping-identity:%'")
        {
            self.counter
                .evidence_reads
                .set(self.counter.evidence_reads.get().saturating_add(1));
        }

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
        if let Some(hook) = self.counter.before_transaction.borrow_mut().take() {
            hook().map_err(X::from)?;
        }
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
        sql.contains("SELECT \"rowid\" AS _migration_rowid")
            && sql.contains("ORDER BY \"rowid\" LIMIT ?1"),
        "a `rowid` property column (`p:rowid`) must not shadow the hidden rowid: {sql}"
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
            ("p:name", payload, true)
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
        .commit_with_partition_postings(commit.clone(), std::slice::from_ref(&quantity), "first")
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
        cell.commit_with_partition_postings(
            commit.clone(),
            std::slice::from_ref(&quantity),
            "first"
        )
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
        cell.commit_with_partition_postings(
            changed_expectation,
            std::slice::from_ref(&quantity),
            "first"
        ),
        Err(norn_kernel::CommitError::Host(
            CellError::IdempotencyConflict(_)
        ))
    ));
    let mut changed = commit.clone();
    if let FactMutation::Put { values } = &mut changed.writes[0].mutation {
        values.insert("tag".into(), Value::String("later-default".into()));
    }
    assert!(matches!(
        cell.commit_with_partition_postings(changed, std::slice::from_ref(&quantity), "first"),
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
            .commit_with_partition_postings(commit, std::slice::from_ref(&quantity), "first")
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

const RESERVED_PROPERTY_IDS: [&str; 11] = [
    "rowid",
    "oid",
    "_rowid_",
    "_world",
    "_id",
    "_type",
    "_version",
    "_scope",
    "_overflow",
    "_source",
    "_target",
];

/// Every SQLite keyword (<https://sqlite.org/lang_keywords.html>), lowercased
/// to fit the property id grammar.
const SQLITE_KEYWORDS: [&str; 147] = [
    "abort",
    "action",
    "add",
    "after",
    "all",
    "alter",
    "always",
    "analyze",
    "and",
    "as",
    "asc",
    "attach",
    "autoincrement",
    "before",
    "begin",
    "between",
    "by",
    "cascade",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "commit",
    "conflict",
    "constraint",
    "create",
    "cross",
    "current",
    "current_date",
    "current_time",
    "current_timestamp",
    "database",
    "default",
    "deferrable",
    "deferred",
    "delete",
    "desc",
    "detach",
    "distinct",
    "do",
    "drop",
    "each",
    "else",
    "end",
    "escape",
    "except",
    "exclude",
    "exclusive",
    "exists",
    "explain",
    "fail",
    "filter",
    "first",
    "following",
    "for",
    "foreign",
    "from",
    "full",
    "generated",
    "glob",
    "group",
    "groups",
    "having",
    "if",
    "ignore",
    "immediate",
    "in",
    "index",
    "indexed",
    "initially",
    "inner",
    "insert",
    "instead",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "last",
    "left",
    "like",
    "limit",
    "match",
    "materialized",
    "natural",
    "no",
    "not",
    "nothing",
    "notnull",
    "null",
    "nulls",
    "of",
    "offset",
    "on",
    "or",
    "order",
    "others",
    "outer",
    "over",
    "partition",
    "plan",
    "pragma",
    "preceding",
    "primary",
    "query",
    "raise",
    "range",
    "recursive",
    "references",
    "regexp",
    "reindex",
    "release",
    "rename",
    "replace",
    "restrict",
    "returning",
    "right",
    "rollback",
    "row",
    "rows",
    "savepoint",
    "select",
    "set",
    "table",
    "temp",
    "temporary",
    "then",
    "ties",
    "to",
    "transaction",
    "trigger",
    "unbounded",
    "union",
    "unique",
    "update",
    "using",
    "vacuum",
    "values",
    "view",
    "virtual",
    "when",
    "where",
    "window",
    "with",
    "without",
];

fn reserved_value(id: &str, generation: u32) -> Value {
    Value::String(format!("{id}-{generation}"))
}

fn put_reserved(generation: u32) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: "defs.Item".to_owned(),
        key: "reserved".to_owned(),
        values: RESERVED_PROPERTY_IDS
            .iter()
            .map(|id| ((*id).to_owned(), reserved_value(id, generation)))
            .collect(),
    }
}

fn binding_lookups(store: &CellStore, key: &[(String, Value)], sort_property: &str) {
    let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    let mut budget = DecodeBudget::new(&limits);
    let (rows, complete) = store
        .read()
        .binding_key_rows_with_budget(MAIN_WORLD, "org_demo/dev", "defs.Item", key, 1, &mut budget)
        .expect("identity key lookup");
    assert!(complete);
    assert_eq!(rows.len(), 1, "identity key index finds the object");
    let mut budget = DecodeBudget::new(&limits);
    let candidate_bytes = budget.remaining_bytes();
    let candidate = store
        .read()
        .binding_sorted_candidate_after_with_budget(
            MAIN_WORLD,
            "defs.Item",
            sort_property,
            false,
            None,
            &mut budget,
            candidate_bytes,
        )
        .expect("binding sort index is usable")
        .expect("candidate fits");
    assert_eq!(
        candidate.identity.map(|(_, object_type)| object_type),
        Some("defs.Item".to_owned())
    );
}

fn table_columns(engine: &NativeEngine, table: &str) -> Vec<String> {
    engine
        .query(
            "SELECT \"name\" FROM pragma_table_info(?1) ORDER BY \"cid\"",
            &[SqlValue::from(table)],
        )
        .expect("table columns")
        .into_iter()
        .map(|row| match row.first() {
            Some(SqlValue::Text(name)) => name.clone(),
            other => panic!("column name: {other:?}"),
        })
        .collect()
}

fn index_sql(engine: &NativeEngine, index: &str) -> String {
    engine
        .query_row(
            "SELECT \"sql\" FROM \"sqlite_master\" WHERE \"name\" = ?1",
            &[SqlValue::from(index)],
            |row| row.get::<String>(0),
        )
        .expect("index exists")
}

#[test]
fn property_ids_naming_rowid_aliases_and_system_columns_store_read_write_and_index() {
    let properties: Vec<serde_json::Value> = RESERVED_PROPERTY_IDS
        .iter()
        .map(|id| json!({"id":id, "type":{"kind":"text"}}))
        .collect();
    let definition = serde_json::from_value(json!({
        "name":"defs.Item",
        "properties":properties,
        "identity":[{"name":"by_rowid", "properties":["rowid", "_id"]}]
    }))
    .expect("object type decodes");
    let model = Model::from_definitions_with_binding_indexes(
        &[definition],
        &[],
        &[],
        &[BindingIndexSpec {
            object_type: "defs.Item".to_owned(),
            property_id: "oid".to_owned(),
            sort: true,
            filter: true,
        }],
    )
    .expect("reserved property ids project");
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("reserved.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("opens");
    store.commit(&batch(put_reserved(1), 1)).expect("creates");
    store.commit(&batch(put_reserved(2), 2)).expect("replaces");
    drop(store);

    let store = CellStore::open(&path, model, StoreOptions::default()).expect("reopens");
    let object = store
        .read()
        .object(MAIN_WORLD, "defs.Item", "reserved")
        .expect("reads")
        .expect("object exists");
    for id in RESERVED_PROPERTY_IDS {
        assert_eq!(object.values.get(id), Some(&reserved_value(id, 2)), "{id}");
    }
    let key = ["rowid", "_id"].map(|id| (id.to_owned(), reserved_value(id, 2)));
    binding_lookups(&store, &key, "oid");
    drop(store);

    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
    let mut expected = ["_world", "_id", "_type", "_scope", "_version"]
        .map(str::to_owned)
        .to_vec();
    expected.extend(RESERVED_PROPERTY_IDS.map(|id| format!("p:{id}")));
    assert_eq!(table_columns(&engine, "o:defs.Item"), expected);
}

#[test]
fn sqlite_keywords_as_property_ids_store_read_write_and_index_in_generated_columns() {
    // Chunks stay under the per-table column budget, so every keyword gets a
    // physical column instead of overflowing into JSON.
    for (chunk_index, chunk) in SQLITE_KEYWORDS.chunks(50).enumerate() {
        let properties: Vec<serde_json::Value> = chunk
            .iter()
            .map(|id| json!({"id":id, "type":{"kind":"text"}}))
            .collect();
        let definition = serde_json::from_value(json!({
            "name":"defs.Item",
            "properties":properties,
            "identity":[{"name":"by_keywords", "properties":[chunk[0], chunk[1]]}]
        }))
        .expect("object type decodes");
        let model = Model::from_definitions_with_binding_indexes(
            &[definition],
            &[],
            &[],
            &[BindingIndexSpec {
                object_type: "defs.Item".to_owned(),
                property_id: chunk[2].to_owned(),
                sort: true,
                filter: true,
            }],
        )
        .expect("keyword property ids project");
        let put = |generation: u32| FactWriteRow::Put {
            object_type: "defs.Item".to_owned(),
            key: "keywords".to_owned(),
            values: chunk
                .iter()
                .map(|id| ((*id).to_owned(), reserved_value(id, generation)))
                .collect(),
        };
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory
            .path()
            .join(format!("keywords-{chunk_index}.sqlite"));
        let mut store =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("opens");
        store.commit(&batch(put(1), 1)).expect("creates");
        store.commit(&batch(put(2), 2)).expect("replaces");
        drop(store);

        let store = CellStore::open(&path, model, StoreOptions::default()).expect("reopens");
        let object = store
            .read()
            .object(MAIN_WORLD, "defs.Item", "keywords")
            .expect("reads")
            .expect("object exists");
        for id in chunk {
            assert_eq!(object.values.get(*id), Some(&reserved_value(id, 2)), "{id}");
        }
        let key = [chunk[0], chunk[1]].map(|id| (id.to_owned(), reserved_value(id, 2)));
        binding_lookups(&store, &key, chunk[2]);
        drop(store);

        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
        let property_columns: Vec<String> = table_columns(&engine, "o:defs.Item")
            .into_iter()
            .filter(|column| !column.starts_with('_'))
            .collect();
        let expected: Vec<String> = chunk.iter().map(|id| format!("p:{id}")).collect();
        assert_eq!(property_columns, expected);
    }
}

#[test]
fn mixed_or_incomplete_property_column_layouts_are_refused_and_left_untouched() {
    let definition = serde_json::from_value(json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"code", "type":{"kind":"text"}},
            {"id":"note", "type":{"kind":"text"}, "constraints":{"nullable":true}}
        ],
        "identity":[{"name":"by_code", "properties":["code"]}]
    }))
    .expect("object type decodes");
    let model = Model::from_definitions(&[definition], &[]).expect("model projects");
    let directory = tempfile::tempdir().expect("temp directory");
    // Mixed: one raw column beside two generated ones. Incomplete: the raw
    // layout with a property column missing.
    let cases: [(&str, &[&str]); 2] = [
        (
            "mixed",
            &["ALTER TABLE \"o:defs.Item\" RENAME COLUMN \"p:name\" TO \"name\""],
        ),
        (
            "incomplete",
            &[
                "ALTER TABLE \"o:defs.Item\" RENAME COLUMN \"p:name\" TO \"name\"",
                "ALTER TABLE \"o:defs.Item\" RENAME COLUMN \"p:code\" TO \"code\"",
                "ALTER TABLE \"o:defs.Item\" DROP COLUMN \"p:note\"",
            ],
        ),
    ];
    for (case, edits) in cases {
        let path = directory.path().join(format!("{case}.sqlite"));
        drop(CellStore::open(&path, model.clone(), StoreOptions::default()).expect("creates"));
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
        for edit in edits {
            engine.execute_batch(edit).expect("edits the layout");
        }
        engine
            .execute_batch("DELETE FROM \"_norn_meta\" WHERE \"key\" = 'property_columns'")
            .expect("drops the layout marker");
        let before = table_columns(&engine, "o:defs.Item");
        drop(engine);

        let refused = CellStore::open(&path, model.clone(), StoreOptions::default());
        assert!(
            matches!(refused, Err(CellError::SchemaMismatch { .. })),
            "{case}: {:?}",
            refused.err()
        );
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
        assert_eq!(
            table_columns(&engine, "o:defs.Item"),
            before,
            "{case}: no rename kept"
        );
        let marker = engine
            .query(
                "SELECT \"value\" FROM \"_norn_meta\" WHERE \"key\" = 'property_columns'",
                &[],
            )
            .expect("reads meta");
        assert!(
            marker.is_empty(),
            "{case}: the layout marker is not written"
        );
    }
}

#[test]
fn raw_named_property_columns_are_renamed_on_open_with_data_and_indexes() {
    let definition = serde_json::from_value(json!({
        "name":"defs.Item",
        "properties":[
            {"id":"name", "type":{"kind":"text"}},
            {"id":"code", "type":{"kind":"text"}},
            {"id":"note", "type":{"kind":"text"}, "constraints":{"nullable":true}}
        ],
        "identity":[{"name":"by_code", "properties":["code"]}]
    }))
    .expect("object type decodes");
    let model = Model::from_definitions_with_binding_indexes(
        &[definition],
        &[],
        &[],
        &[BindingIndexSpec {
            object_type: "defs.Item".to_owned(),
            property_id: "name".to_owned(),
            sort: true,
            filter: true,
        }],
    )
    .expect("model projects");
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("legacy.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("opens");
    let values = |code: &str| {
        BTreeMap::from([
            ("name".to_owned(), Value::String(format!("name {code}"))),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("note".to_owned(), Value::String(format!("note {code}"))),
        ])
    };
    let write = |key: &str, code: &str| FactWriteRow::Put {
        object_type: "defs.Item".to_owned(),
        key: key.to_owned(),
        values: values(code),
    };
    store
        .commit(&batch(write("legacy", "A-1"), 1))
        .expect("commits");
    let head = store.read().verify_ledger().expect("ledger verifies");
    drop(store);

    // Rewrite the file into the raw-id layout older binaries created.
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
    for id in ["name", "code", "note"] {
        engine
            .execute_batch(&format!(
                "ALTER TABLE \"o:defs.Item\" RENAME COLUMN \"p:{id}\" TO \"{id}\""
            ))
            .expect("restores the raw column name");
    }
    engine
        .execute_batch("DELETE FROM \"_norn_meta\" WHERE \"key\" = 'property_columns'")
        .expect("drops the layout marker");
    assert!(index_sql(&engine, "o:defs.Item/key:by_code").ends_with("\"_world\", \"code\")"));
    drop(engine);

    let mut store = CellStore::open(&path, model, StoreOptions::default()).expect("upgrades");
    assert_eq!(store.read().verify_ledger().expect("ledger verifies"), head);
    let object = store
        .read()
        .object(MAIN_WORLD, "defs.Item", "legacy")
        .expect("reads")
        .expect("object survives");
    assert_eq!(object.values, values("A-1"));
    binding_lookups(
        &store,
        &[("code".to_owned(), Value::String("A-1".to_owned()))],
        "name",
    );
    store
        .commit(&batch(write("fresh", "B-2"), 2))
        .expect("writes after upgrade");
    assert!(
        store.commit(&batch(write("duplicate", "A-1"), 3)).is_err(),
        "the renamed identity index still enforces uniqueness"
    );
    drop(store);

    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine opens");
    let columns = table_columns(&engine, "o:defs.Item");
    assert!(
        ["p:name", "p:code", "p:note"]
            .iter()
            .all(|column| columns.iter().any(|found| found == column))
            && !columns
                .iter()
                .any(|found| ["name", "code", "note"].contains(&found.as_str())),
        "{columns:?}"
    );
    assert!(index_sql(&engine, "o:defs.Item/key:by_code").ends_with("\"_world\", \"p:code\")"));
    assert!(index_sql(&engine, "o:defs.Item/binding-property/name/asc").contains("\"p:name\" ASC"));
}

#[test]
fn adding_an_interface_parent_scans_existing_rows_before_activating_its_invariants() {
    let mut snapshot = interface_total_invariant_model(false).definitions_snapshot();
    let parent: InterfaceDef = serde_json::from_value(json!({"name":"defs.ParentTotal",
        "properties":[{"id":"total","type":{"kind":"integer"}}],
        "invariants":[{"id":"non_negative_total","expression":"self.total >= 0","message":"nonnegative"}]
    })).expect("parent");
    snapshot.interfaces.push(parent.clone());
    let original = Model::from_snapshot(&snapshot).expect("original model");
    snapshot.interfaces[0].extends = vec![parent.name.clone()];
    let candidate = Model::from_snapshot(&snapshot).expect("candidate model");
    assert_ne!(original.model_hash(), candidate.model_hash());
    for total in [-1, 1] {
        let mut store = CellStore::open_in_memory(original.clone()).expect("store");
        store
            .install_definitions(
                original.clone(),
                install_input("parent-v1", 1, &original),
                Some(0),
                "org_demo/dev",
            )
            .expect("seed");
        store
            .commit(&batch(put_total("defs.Item", "existing", total), 2))
            .expect("existing data");
        let result = store.install_definitions(
            candidate.clone(),
            install_input("parent-v2", 3, &candidate),
            Some(1),
            "org_demo/dev",
        );
        if total < 0 {
            assert!(
                matches!(result, Err(CellError::DefinitionInvariantViolation { violations, .. })
                if violations.iter().any(|finding| finding.owner == norn_ir::InvariantOwner::Interface(parent.name.clone())))
            );
        } else {
            result.expect("valid existing data accepts inherited integrity contract");
        }
    }
}

fn governing_law_input(expression: &str, at: u64) -> (Model, InstalledDefinitionInput) {
    let plain = model(false);
    let mut input = install_input("governing-laws", at, &plain);
    let mut artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_str(&input.canonical_set).expect("artifacts");
    artifacts.push(serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"defs.name.law", "kind":"law",
        "namespace":"defs", "name":"name", "version":"0.1.0", "lifecycle":"draft",
        "scope":{"orgId":"org_demo", "environmentId":"dev"},
        "dependencies":[{"artifactId":"defs.Item.entity", "kind":"entity", "version":"0.1.0", "required":true}],
        "provenance":{"source":"authoring_sdk"},
        "payload":{
            "lawVersion":"0.1", "id":"defs.name", "owner":"defs.controls", "severity":"error",
            "rationale":"Observed names meet the declared contract", "provenance":{"source":"authoring_sdk"},
            "subject":"defs.Item", "scope":{"root":{"kind":"entity", "id":"defs.Item"}, "objectTypes":["defs.Item"]},
            "modality":{"kind":"always"}, "response":{"kind":"predicate", "expression":expression}
        }
    })).expect("law"));
    let findings = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(findings.is_empty(), "{findings:?}");
    input.canonical_set = String::from_utf8(
        norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical set"),
    )
    .expect("UTF8");
    (
        plain
            .with_governing_laws(&artifacts)
            .expect("retained laws"),
        input,
    )
}

#[test]
fn obs_evidence_historical_governing_laws_do_not_follow_current_definitions() {
    let (first, input) = governing_law_input("self.name != 'bad'", 1);
    let expected_first = first.governing_laws("defs.Item").expect("direct laws");
    let mut store = CellStore::open_in_memory(first.clone()).expect("store");
    store
        .install_definitions(first, input, Some(0), "org_demo/dev")
        .expect("v1");
    let (second, input) = governing_law_input("self.name != 'bad' and self.name != 'worse'", 2);
    let expected_second = second.governing_laws("defs.Item").expect("direct laws");
    store
        .install_definitions(second, input, Some(1), "org_demo/dev")
        .expect("v2");
    assert_ne!(expected_first, expected_second);
    assert_eq!(
        store
            .read()
            .governing_laws_at(1, "defs.Item")
            .expect("v1 lookup"),
        expected_first
    );
    assert_eq!(
        store
            .read()
            .governing_laws_at(2, "defs.Item")
            .expect("v2 lookup"),
        expected_second
    );
    assert!(store.read().governing_laws_at(99, "defs.Item").is_err());
}

#[test]
fn obs_evidence_install_refuses_missing_or_substituted_governing_laws() {
    let (retained, input) = governing_law_input("self.name != 'bad'", 1);
    let mut store = CellStore::open_in_memory(model(false)).expect("store");
    for candidate in [
        model(false),
        governing_law_input("self.name != 'other'", 1).0,
    ] {
        assert!(matches!(
            store.install_definitions(candidate, input.clone(), Some(0), "org_demo/dev"),
            Err(CellError::DefinitionReceiptInvalid(_) | CellError::LegacyLawSnapshot)
        ));
        assert!(
            store
                .read()
                .installed_definitions(None)
                .expect("active version")
                .is_none()
        );
    }
    store
        .install_definitions(retained, input, Some(0), "org_demo/dev")
        .expect("matching laws install");
}

#[test]
fn law_interface_inherited_scope_governs_and_effective_scopes_refuse_until_b2() {
    let (model, _) = governing_law_input("self.name != 'bad'", 1);
    let mut snapshot = model.definitions_snapshot();
    snapshot.object_types.push(
        serde_json::from_value(json!({"name":"defs.Child", "extends":"defs.Item"})).expect("child"),
    );
    let inherited = Model::from_snapshot(&snapshot).expect("snapshot");
    assert_eq!(
        inherited
            .governing_laws("defs.Child")
            .expect("LAW-INTERFACE-1: the supertype law governs its subtype"),
        inherited.governing_laws("defs.Item").expect("direct scope")
    );
    let mut mixed = serde_json::to_value(&snapshot.law_artifacts[0]).expect("law");
    mixed["payload"]["scope"]["objectTypes"] = json!(["defs.Item", "defs.Child"]);
    let mut mixed_snapshot = snapshot.clone();
    mixed_snapshot.law_artifacts[0] = serde_json::from_value(mixed).expect("mixed scope");
    let refusal = Model::from_snapshot(&mixed_snapshot)
        .expect("model")
        .governing_laws("defs.Child")
        .expect_err("mixed scope");
    assert!(matches!(refusal, CellError::UnsupportedLawScope { .. }));
    assert!(
        refusal
            .to_string()
            .contains("core.laws.unsupported_semantics")
    );
    let mut artifact = serde_json::to_value(&snapshot.law_artifacts[0]).expect("law");
    artifact["payload"]["scope"]["root"] = json!({"kind":"app", "id":"defs"});
    snapshot.law_artifacts[0] = serde_json::from_value(artifact).expect("effective scope");
    assert!(matches!(
        Model::from_snapshot(&snapshot)
            .expect("model")
            .governing_laws("defs.Item"),
        Err(CellError::UnsupportedLawScope { .. })
    ));
}

norn_testkit::proptest::proptest! {
    #[test]
    fn obs_evidence_snapshot_roundtrip_preserves_generated_governing_identity(name in "[a-z]{1,12}") {
        use norn_testkit::proptest::prelude::*;
        let (model, _) = governing_law_input(&format!("self.name != '{name}'"), 1);
        let snapshot = model.definitions_snapshot();
        let decoded = serde_json::from_slice(&serde_json::to_vec(&snapshot).expect("encode")).expect("decode");
        let restored = Model::from_snapshot(&decoded).expect("restore");
        prop_assert_eq!(restored.model_hash(), model.model_hash());
        prop_assert_eq!(restored.governing_laws("defs.Item").expect("laws"), model.law_artifacts());
        let (changed, _) = governing_law_input(&format!("self.name != '{name}x'"), 1);
        prop_assert_ne!(changed.model_hash(), model.model_hash());
        prop_assert_eq!(changed.ddl_hash(), model.ddl_hash());
    }
}

fn purity_quality_host(
    path: &std::path::Path,
) -> norn_cell::SqliteCell<
    norn_kernel::ObservationEvent,
    norn_kernel::cell_host::JsonObject,
    norn_kernel::cell_host::JsonObject,
> {
    let object = json!({"name":"quality.Measurement", "properties":[{"id":"purity", "type":{"kind":"decimal", "scale":6}}]});
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_value(json!([
        {"irVersion":"0.1", "artifactId":"quality.Measurement.entity", "kind":"entity", "namespace":"quality", "name":"Measurement", "version":"0.1.0", "lifecycle":"draft", "scope":{"orgId":"org_demo", "environmentId":"dev"}, "dependencies":[], "provenance":{"source":"authoring_sdk"}, "payload":{"entityVersion":"2.0", "objectType":object}},
        {"irVersion":"0.1", "artifactId":"quality.purity.law", "kind":"law", "namespace":"quality", "name":"purity", "version":"0.1.0", "lifecycle":"draft", "scope":{"orgId":"org_demo", "environmentId":"dev"}, "dependencies":[{"artifactId":"quality.Measurement.entity", "kind":"entity", "version":"0.1.0", "required":true}], "provenance":{"source":"authoring_sdk"}, "payload":{
            "lawVersion":"0.1", "id":"purity_in_range", "owner":"quality.controls", "severity":"error", "rationale":"Purity must be within zero and one", "provenance":{"source":"authoring_sdk"}, "subject":"quality.Measurement", "scope":{"root":{"kind":"entity", "id":"quality.Measurement"}, "objectTypes":["quality.Measurement"]}, "modality":{"kind":"always"}, "response":{"kind":"predicate", "expression":"self.purity >= 0 and self.purity <= 1"}
        }}
    ])).expect("typed synthetic declarations");
    assert!(
        norn_checker::check_artifact_set(
            &artifacts,
            &norn_checker::EntityArtifactSetContext {
                registry: &std::collections::BTreeSet::new(),
                base: &[],
                tenant: None
            }
        )
        .is_empty()
    );
    let model =
        Model::from_definitions(&[serde_json::from_value(object).expect("measurement")], &[])
            .expect("model")
            .with_governing_laws(&artifacts)
            .expect("laws");
    let mut input = install_input("quality", 1_793_527_260_000, &model);
    input.canonical_set = String::from_utf8(
        norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical set"),
    )
    .expect("UTF8");
    let mut store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("store");
    store
        .install_definitions(model, input, Some(0), "org_demo/dev")
        .expect("admitted definitions");
    norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("host")
}

fn capture_purity(
    cell: &mut norn_cell::SqliteCell<
        norn_kernel::ObservationEvent,
        norn_kernel::cell_host::JsonObject,
        norn_kernel::cell_host::JsonObject,
    >,
    delivery: &str,
    purity: &str,
) {
    capture_purity_with_retry(cell, delivery, purity, true);
}

fn capture_purity_with_retry(
    cell: &mut norn_cell::SqliteCell<
        norn_kernel::ObservationEvent,
        norn_kernel::cell_host::JsonObject,
        norn_kernel::cell_host::JsonObject,
    >,
    delivery: &str,
    purity: &str,
    check_retry: bool,
) {
    use norn_kernel::snapshot_observe::{ObservedSnapshot, commit_observed_snapshot};
    use norn_kernel::{
        CellHost, DecisionTurn, FactKey, ObservationId, Observed, Provenance, TypeCatalog, World,
        load_slice,
    };
    let values = BTreeMap::from([(
        "purity".to_owned(),
        Value::from_json_text(purity).expect("exact decimal"),
    )]);
    let provenance: norn_ir::InstanceProvenance = serde_json::from_value(json!({"source":{"system":"synthetic-erpnext", "record":"Stock Entry Measurement/MEAS-1"}, "mappingVersion":"synthetic.measurements@1", "recordedAt":"2026-11-01T10:01:00Z", "sourceFields":["purity"]})).expect("source provenance");
    let observed = Observed {
        id: ObservationId(delivery.to_owned()),
        provenance: Provenance {
            source: "synthetic-erpnext".into(),
            source_id: "Stock Entry Measurement/MEAS-1".into(),
            observed_at: cell.now(),
            mapping_version: "synthetic.measurements@1".into(),
        },
        input: BTreeMap::from([
            ("values".into(), Value::Object(values.clone())),
            (
                "provenance".into(),
                norn_ir::runtime::encode(&provenance).expect("provenance"),
            ),
        ]),
    };
    assert!(cell.inject(&observed).expect("capture"));
    assert!(!cell.inject(&observed).expect("same source delivery"));
    let pending = cell.observe().expect("observe").expect("pending");
    let model = cell.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types()).with_governing_laws(model.law_artifacts());
    let key = FactKey::new("quality.Measurement", "MEAS-1");
    let slice = load_slice(cell, [key.clone()]).expect("slice");
    let turn = DecisionTurn {
        id: delivery.into(),
        now: cell.now(),
        world: World::Main,
        consumes: Some(pending.consumed()),
    };
    let snapshot = ObservedSnapshot {
        key: &key,
        values: &values,
        lifecycles: &[],
        provenance: &provenance,
    };
    commit_observed_snapshot(cell, &catalog, &slice, &turn, snapshot)
        .expect("source violation remains captured");
    if !check_retry {
        return;
    }
    let retry = commit_observed_snapshot(cell, &catalog, &slice, &turn, snapshot)
        .expect_err("consumed delivery");
    assert!(matches!(
        retry,
        norn_kernel::snapshot_observe::ObservationCommitRefusal::Commit(
            norn_kernel::CommitError::NotPending { .. }
        )
    ));
}

norn_testkit::proptest::proptest! {
    #[test]
    fn data_quarantine_redelivery_persists_one_finding_and_correction_recovers(deliveries in 2usize..6, fraction in 1u16..1000) {
        for purity in ["1.20".to_owned(), format!("1.{fraction:03}")] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("quality.sqlite");
        let mut cell = purity_quality_host(&path);
        for delivery in 0..deliveries {
            capture_purity(&mut cell, &format!("delivery-{delivery}"), &purity);
            let model = cell.store().model().clone();
            drop(cell);
            cell = norn_cell::SqliteCell::open(&path, model, StoreOptions::default(), "org_demo/dev", |_| BTreeMap::new()).expect("restart");
            let reader = cell.store().read();
            let raw = reader.object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("raw").expect("captured");
            norn_testkit::proptest::prop_assert!(reader.observation_is_quarantined(MAIN_WORLD, &raw).expect("quarantine"));
            norn_testkit::proptest::prop_assert!(reader.authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_none());
            let page = reader.authoritative_page(MAIN_WORLD, "quality.Measurement", None, 10).expect("authoritative projection");
            norn_testkit::proptest::prop_assert!(page.records.is_empty());
            norn_testkit::proptest::prop_assert_eq!(page.quarantine_count, 1);
            norn_testkit::proptest::prop_assert_eq!(page.scanned_through.as_deref(), Some("MEAS-1"));
            let findings = reader.observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("findings");
            norn_testkit::proptest::prop_assert_eq!(findings.len(), 1);
            norn_testkit::proptest::prop_assert_eq!(findings[0].status, norn_ir::DataQualityFindingStatus::Open);
        }
        capture_purity(&mut cell, "corrected", "0.80");
        let reader = cell.store().read();
        norn_testkit::proptest::prop_assert!(reader.authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_some());
        let findings = reader.observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("review");
        norn_testkit::proptest::prop_assert_eq!(findings[0].status, norn_ir::DataQualityFindingStatus::Resolved);
        let persisted: Vec<_> = reader.ledger_page(0, 100).expect("ledger").into_iter().filter(|event| event.event.get("op").and_then(Value::as_str) == Some("data_quality_finding")).collect();
        norn_testkit::proptest::prop_assert_eq!(persisted.len(), 1);
        norn_testkit::proptest::prop_assert_eq!(persisted[0].event.get("fact").and_then(|fact| fact.get("status")).and_then(Value::as_str), Some("open"));
        }
    }
}

norn_testkit::proptest::proptest! {
#[test]
fn obs_recovery_upgrade_uses_each_findings_retained_model(fraction in 501u16..1000) {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("upgrade.sqlite");
    let mut cell = purity_quality_host(&path);
    capture_purity(&mut cell, "original", "1.20");
    let original = cell.store().read().observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("findings")[0].clone();
    let previous = cell.store().read().installed_definitions(None).expect("definitions").expect("installed");
    let mut artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(&previous.canonical_set).expect("artifacts");
    let mut snapshot = cell.store().model().definitions_snapshot();
    let mut law = serde_json::to_value(&artifacts[1]).expect("law");
    law["version"] = json!("0.2.0");
    law["payload"]["response"]["expression"] = json!("self.purity >= 0 and self.purity <= 0.5");
    artifacts[1] = serde_json::from_value(law).expect("tightened law");
    snapshot.law_artifacts = vec![artifacts[1].clone()];
    let model = Model::from_snapshot(&snapshot).expect("retained candidate");
    let mut input = install_input("tightened", previous.installed_at.checked_add(1).expect("next epoch"), &model);
    input.canonical_set = String::from_utf8(norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical set")).expect("UTF8");
    let old_model = cell.store().model().clone();
    drop(cell);
    let mut store = CellStore::open(&path, old_model, StoreOptions::default()).expect("store");
    store.install_definitions(model, input, Some(previous.version), "org_demo/dev").expect("upgrade");
    let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    let review = |cell: &norn_cell::SqliteCell<norn_kernel::ObservationEvent, norn_kernel::cell_host::JsonObject, norn_kernel::cell_host::JsonObject>| cell.store().read().observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("version-bound review");
    assert_eq!(review(&cell)[0].status, norn_ir::DataQualityFindingStatus::Open);
    capture_purity(&mut cell, "still-invalid", "1.30");
    assert!(review(&cell).iter().all(|f| f.status == norn_ir::DataQualityFindingStatus::Open));
    capture_purity(&mut cell, "passes-old-law", &format!("0.{fraction:03}"));
    let findings = review(&cell);
    assert_eq!(findings.iter().find(|f| f.id == original.id).expect("original retained").status, norn_ir::DataQualityFindingStatus::Resolved);
    assert!(findings.iter().any(|f| f.status == norn_ir::DataQualityFindingStatus::Open));
    assert!(cell.store().read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("still quarantined").is_none());
    capture_purity(&mut cell, "passes-both-laws", "0.40");
    assert!(review(&cell).iter().all(|f| f.status == norn_ir::DataQualityFindingStatus::Resolved));
    assert!(cell.store().read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("recovered").is_some());
    let original_event = cell.store().read().ledger_page(0, 100).expect("history").into_iter().find(|e| e.event.get("fact").and_then(|f| f.get("id")).and_then(Value::as_str) == Some(original.id.as_str())).expect("original event");
    assert_eq!(original_event.event.get("fact").and_then(|f| f.get("status")).and_then(Value::as_str), Some("open"));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("cache connection");
    engine.execute(&format!("UPDATE {} SET {} = {} + 1", quote_identifier("o:quality.Measurement"), quote_identifier(VERSION_COLUMN), quote_identifier(VERSION_COLUMN)), &[]).expect("unproven mutable version");
    assert!(cell.store().read().observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").is_err(), "unproven correction refuses");
}

}

#[test]
fn obs_bad_scrap_yield_purity_120_is_captured_but_native_write_refuses() {
    use norn_kernel::{CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite};
    let directory = tempfile::tempdir().expect("directory");
    let mut cell = purity_quality_host(&directory.path().join("quality.sqlite"));
    let mut native = CellCommit::empty();
    native.writes.push(FactWrite {
        key: FactKey::new("quality.Measurement", "MEAS-1"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: BTreeMap::from([(
                "purity".into(),
                Value::from_json_text("1.20").expect("decimal"),
            )]),
        },
    });
    let refusal = cell.commit(native).expect_err("native invalid purity");
    assert_eq!(refusal.code(), "core.laws.violated");
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("raw")
            .is_none()
    );
    capture_purity(&mut cell, "purity-120", "1.20");
    let findings = cell
        .store()
        .read()
        .observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("persisted findings");
    assert_eq!(findings.len(), 1);
    assert!(
        matches!(&findings[0].rule_ref, norn_ir::DataQualityRuleRef::Law {law_id, ..} if law_id == "purity_in_range")
    );
}

// OBS-EVIDENCE-1: local delivery/restart metadata cannot duplicate a persisted finding.
norn_testkit::proptest::proptest! {
    #[test]
    fn obs_evidence_redelivery_persists_one_law_finding_after_restart(deliveries in 2usize..6, fraction in 1u16..1000) {
        for purity in ["1.20".to_owned(), format!("1.{fraction:03}")] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("quality.sqlite");
            let mut cell = purity_quality_host(&path);
            let mut id = None;
            for delivery in 0..deliveries {
                capture_purity(&mut cell, &format!("delivery-{delivery}"), &purity);
                let model = cell.store().model().clone();
                drop(cell);
                cell = norn_cell::SqliteCell::open(&path, model, StoreOptions::default(), "org_demo/dev", |_| BTreeMap::new()).expect("restart");
                let reader = cell.store().read();
                let findings: Vec<_> = reader.ledger_page(0, 100).expect("ledger").into_iter().filter(|event| event.event.get("op").and_then(Value::as_str) == Some("data_quality_finding")).collect();
                norn_testkit::proptest::prop_assert_eq!(findings.len(), 1);
                let verified = reader.verified_history_event(&findings[0]).expect("proof").expect("retained producer");
                let finding: norn_ir::DataQualityFinding = norn_ir::runtime::decode(verified.get("fact").expect("fact")).expect("finding");
                norn_testkit::proptest::prop_assert!(matches!(&finding.rule_ref, norn_ir::DataQualityRuleRef::Law {law_id, ..} if law_id == "purity_in_range"), "retained purity law pin");
                if let Some(ref before) = id { norn_testkit::proptest::prop_assert_eq!(before, &finding.id); }
                id = Some(finding.id);
                norn_testkit::proptest::prop_assert!(reader.object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("raw").is_some());
            }
        }
    }
}

#[test]
fn obs_evidence_idempotent_capture_reuses_the_original_finding_cutoff() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cutoff.sqlite");
    let mut cell = purity_quality_host(&path);
    capture_purity(&mut cell, "initial", "1.20");
    let model = cell.store().model().clone();
    let original = cell
        .store()
        .read()
        .ledger_page(0, 20)
        .expect("ledger")
        .into_iter()
        .find(|event| event.event.get("op").and_then(Value::as_str) == Some("data_quality_finding"))
        .expect("finding");
    let original: norn_ir::DataQualityFinding =
        norn_ir::runtime::decode(original.event.get("fact").expect("fact")).expect("decode");
    let finding = norn_ir::DataQualityFinding::v2(
        norn_ir::DataQualityRuleRef::Builtin {
            id: "test.capture.retry".into(),
            version: "1".into(),
        },
        original.subjects,
        original.severity,
        original.message,
        original.outcome,
        original.provenance,
        original.first_seen,
        original.last_seen,
        original.status,
    )
    .expect("source finding")
    .with_evidence_digest(original.evidence_digest.expect("digest"))
    .expect("identity");
    drop(cell);
    let mut store = CellStore::open(&path, model, StoreOptions::default()).expect("store");
    let fact = norn_ir::runtime::encode(&finding).expect("encoded finding");
    store
        .inject("retry-producer", &fact)
        .expect("committed producer preimage");
    let batch = norn_cell::CommitBatch {
        world: MAIN_WORLD.into(),
        scope: "org_demo/dev".into(),
        logical_time: store.read().clock_floor().expect("clock"),
        idempotency_key: Some("finding-request".into()),
        consumes: Some(norn_cell::Consumption::Observation {
            observation_id: "retry-producer".into(),
        }),
        events: vec![norn_ir::runtime_object! {"op"=>"data_quality_finding", "fact"=>fact}],
        ..norn_cell::CommitBatch::default()
    };
    assert!(matches!(
        store.commit(&batch).expect("first"),
        norn_cell::CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        store.commit(&batch).expect("normalized retry"),
        norn_cell::CommitOutcome::Replayed(_)
    ));
    let retained = store
        .read()
        .ledger_page(0, 100)
        .expect("ledger")
        .into_iter()
        .filter(|entry| {
            entry
                .event
                .get("fact")
                .and_then(|fact| fact.get("id"))
                .and_then(Value::as_str)
                == Some(finding.id.as_str())
        })
        .count();
    assert_eq!(retained, 1);
}

#[test]
fn obs_evidence_legacy_candidates_do_not_block_a_verified_producer() {
    let directory = tempfile::tempdir().expect("directory");
    let mut source = purity_quality_host(&directory.path().join("source.sqlite"));
    capture_purity(&mut source, "source", "1.20");
    let event = source
        .store()
        .read()
        .ledger_page(0, 20)
        .expect("history")
        .into_iter()
        .find(|entry| entry.event.get("op").and_then(Value::as_str) == Some("data_quality_finding"))
        .expect("finding")
        .event;
    let path = directory.path().join("legacy.sqlite");
    let cell = purity_quality_host(&path);
    let model = cell.store().model().clone();
    drop(cell);
    let mut store = CellStore::open(&path, model, StoreOptions::default()).expect("store");
    for index in 0..65 {
        store
            .commit(&norn_cell::CommitBatch {
                world: MAIN_WORLD.into(),
                scope: "org_demo/dev".into(),
                logical_time: store.read().clock_floor().expect("clock"),
                idempotency_key: Some(format!("legacy-{index}")),
                events: vec![event.clone()],
                ..norn_cell::CommitBatch::default()
            })
            .expect("unproven legacy metadata");
    }
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("host");
    capture_purity(&mut cell, "first-proven", "1.20");
    capture_purity(&mut cell, "redelivery", "1.20");
    let reader = cell.store().read();
    let entries = reader.ledger_page(0, 200).expect("history");
    let mut verified = 0;
    for entry in entries.iter().filter(|entry| {
        entry.event.get("op").and_then(Value::as_str) == Some("data_quality_finding")
    }) {
        if reader
            .verified_history_event(entry)
            .expect("proof")
            .is_some()
        {
            verified += 1;
        }
    }
    assert_eq!(verified, 1, "proof search passes the first candidate page");
    assert_eq!(
        reader
            .observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("paged review")
            .len(),
        1
    );
    assert!(
        reader
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("raw")
            .is_some()
    );
}

fn mapping_bundle_store(path: &Path) -> CellStore {
    let cell = purity_quality_host(path);
    let previous = cell
        .store()
        .read()
        .installed_definitions(None)
        .expect("definitions")
        .expect("installed");
    let mut artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_str(&previous.canonical_set).expect("retained");
    let mut entity = serde_json::to_value(&artifacts[0]).expect("entity");
    entity["payload"]["objectType"]["implements"] = json!(["quality.Measured"]);
    entity["dependencies"] = json!([{ "artifactId":"quality.Measured.interface", "kind":"entity", "version":"0.1.0", "required":true }]);
    artifacts[0] = serde_json::from_value(entity.clone()).expect("entity");
    entity["dependencies"] = json!([]);
    let interface = json!({"name":"quality.Measured", "properties":[{"id":"purity", "type":{"kind":"decimal", "scale":6}}]});
    entity["artifactId"] = json!("quality.Measured.interface");
    entity["payload"] = json!({"entityVersion":"2.0", "interface":interface});
    artifacts.push(serde_json::from_value(entity).expect("interface"));
    let mut source: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/synthetic-contact.source_schema.json"
    ))
    .expect("source fixture");
    source["artifactId"] = json!("quality.source");
    source["payload"]["source"] = json!({"system":"synthetic-erpnext", "instance":"demo"});
    source["payload"]["fields"] = json!([
        {"dataset":"Rows", "name":"name", "ordinal":0, "nativeType":"text", "typeCandidate":{"type":"string", "origin":"declared"}, "required":true},
        {"dataset":"Rows", "name":"purity", "ordinal":1, "nativeType":"decimal", "typeCandidate":{"type":"decimal", "origin":"declared"}, "required":true}
    ]);
    let source: norn_ir::ArtifactEnvelope = serde_json::from_value(source).expect("source");
    let pin = norn_checker::ArtifactRef::of(&source).expect("source pin");
    let mut mapping: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/synthetic-rows.mapping.json"
    ))
    .expect("mapping fixture");
    mapping["artifactId"] = json!("quality.mapping");
    mapping["dependencies"] = json!([
        {"artifactId":"quality.source", "kind":"source_schema", "version":"1.0.0", "required":true},
        {"artifactId":"quality.Measurement.entity", "kind":"entity", "version":"0.1.0", "required":true},
        {"artifactId":"quality.Measured.interface", "kind":"entity", "version":"0.1.0", "required":true}
    ]);
    mapping["payload"]["source"] =
        json!({"schemaId":"quality.source", "version":"1.0.0", "sha256":pin.sha256});
    mapping["payload"]["interfaces"] = json!(["quality.Measured.interface"]);
    mapping["payload"]["maps"] = json!([{"id":"measurement", "source":{"dataset":"Rows"}, "target":{"entity":"quality.Measurement.entity", "interface":"quality.Measured.interface"}, "subject":"input.name", "sourceRecord":"input.name", "properties":[{"property":"purity", "value":"input.purity"}]}]);
    artifacts.push(source);
    let mut alternate = mapping.clone();
    alternate["artifactId"] = json!("quality.alternate");
    alternate["payload"]["maps"][0]["subject"] = json!("input.name + '-ALT'");
    artifacts.push(serde_json::from_value(alternate).expect("alternate role"));
    artifacts.push(serde_json::from_value(mapping).expect("mapping"));
    let object =
        serde_json::to_value(&artifacts[0]).expect("object")["payload"]["objectType"].clone();
    let model = Model::from_definitions_with_interfaces(
        &[serde_json::from_value(object).expect("object")],
        &[],
        &[serde_json::from_value(interface).expect("interface")],
    )
    .expect("model")
    .with_governing_laws(&artifacts)
    .expect("laws");
    let findings = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(findings.is_empty(), "{findings:?}");
    let old_model = cell.store().model().clone();
    drop(cell);
    let mut store = CellStore::open(path, old_model, StoreOptions::default()).expect("store");
    let mut installation = install_input(
        "bundle",
        previous.installed_at.checked_add(1).expect("next epoch"),
        &model,
    );
    installation.canonical_set =
        String::from_utf8(norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical"))
            .expect("UTF8");
    store
        .install_definitions(model, installation, Some(previous.version), "org_demo/dev")
        .expect("admission");
    store
}

fn bundle_input() -> norn_cell::mapping_bundle::MappingInput {
    norn_cell::mapping_bundle::MappingInput {
        mapping: "quality.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "demo".into(),
        partition: "measurements".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-01T10:01:00Z".into()),
    }
}
fn bundle_raw(name: &str, purity: &str) -> norn_kernel::Observed<Value> {
    let payload = Value::from_json_text(format!(r#"{{"name":"{name}","purity":{purity}}}"#))
        .expect("payload");
    let hash = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("JSON"))
        .expect("digest");
    norn_kernel::Observed {
        id: norn_kernel::ObservationId("delivery".into()),
        provenance: norn_kernel::Provenance {
            source: "synthetic-erpnext".into(),
            source_id: name.into(),
            mapping_version: "pipeline/1".into(),
            observed_at: norn_kernel::LogicalTime::from_millis(1_793_527_260_000),
        },
        input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"synthetic-erpnext", "source_id"=>name, "event_id"=>"1", "source_version"=>"1", "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_527_260_000u64, "payload"=>payload, "payload_digest"=>hash},
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn mapping_bundle_identity_pins_evidence_and_redelivery(name in "[A-Z]{1,12}", fraction in 1u16..1000, deliveries in 2usize..5) {
        let dir = tempfile::tempdir().expect("directory");
        let mut store = mapping_bundle_store(&dir.path().join("bundle.sqlite"));
        let raw = bundle_raw(&name, &format!("1.{fraction:03}"));
        let input = bundle_input();
        let (id, inserted) = store.inject_mapping_bundle(&raw, &input).expect("capture");
        norn_testkit::proptest::prop_assert!(inserted);
        for delivery in 1..deliveries {
            let mut redelivery = raw.clone();
            redelivery.id.0 = format!("local-delivery-{delivery}");
            redelivery.provenance.observed_at = norn_kernel::LogicalTime::from_millis(u64::try_from(delivery).expect("metadata"));
            let (replayed, inserted) = store.inject_mapping_bundle(&redelivery, &input).expect("replay");
            norn_testkit::proptest::prop_assert_eq!(&replayed, &id);
            norn_testkit::proptest::prop_assert!(!inserted);
        }
        let pending = store.read().pending_observations().expect("inbox");
        norn_testkit::proptest::prop_assert_eq!(pending.len(), 1);
        let observed: norn_kernel::Observed<Value> = norn_ir::runtime::decode(&pending[0].observed).expect("envelope");
        let bundle: norn_cell::mapping_bundle::MappingBundle = norn_ir::runtime::decode(&observed.input).expect("bundle");
        norn_testkit::proptest::prop_assert_eq!(&bundle.raw, &raw.input);
        norn_testkit::proptest::prop_assert_eq!(bundle.records.len(), 1);
        store.read().recheck_mapping_bundle(&bundle).expect("cell recomputation");
        let mut unknown_pin = bundle.clone(); unknown_pin.definitions_version = u64::MAX;
        norn_testkit::proptest::prop_assert!(matches!(store.read().recheck_mapping_bundle(&unknown_pin), Err(CellError::MappingBundleMismatch)), "unretained definition pin refuses stably");
        let mut altered = bundle; altered.records.clear();
        norn_testkit::proptest::prop_assert!(matches!(store.read().recheck_mapping_bundle(&altered), Err(CellError::MappingBundleMismatch)), "derived output refuses");
        let mut changed = input; changed.partition.push('x');
        let (different, _) = store.inject_mapping_bundle(&raw, &changed).expect("other pins");
        norn_testkit::proptest::prop_assert_ne!(different, id);
    }
}

norn_testkit::proptest::proptest! {
    #[test]
    fn mapping_bundle_processing_replay_has_one_bundle_and_finding(deliveries in 2usize..5) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("mapped.sqlite");
        let mut store = mapping_bundle_store(&path);
        let raw = bundle_raw("MEAS-1", "1.20");
        let input = bundle_input();
        let model = store.model().clone();
        for index in 0..deliveries {
            let (id, inserted) = store.inject_mapping_bundle(&raw, &input).expect("capture");
            norn_testkit::proptest::prop_assert_eq!(inserted, index == 0);
            let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
            norn_testkit::proptest::prop_assert_eq!(cell.process_mapping_bundle(&id).expect("processing"), index == 0);
            let reader = cell.store().read();
            let findings = reader.observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("review");
            norn_testkit::proptest::prop_assert_eq!(findings.len(), 1);
            norn_testkit::proptest::prop_assert!(reader.authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_none());
            norn_testkit::proptest::prop_assert!(reader.object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("raw").is_some());
            let persisted = reader.ledger_page(0, 100).expect("history").into_iter().filter(|e| e.event.get("op").and_then(Value::as_str) == Some("data_quality_finding")).count();
            norn_testkit::proptest::prop_assert_eq!(persisted, 1);
            drop(cell);
            store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
        }
    }
}

#[test]
fn mapping_bundle_processing_refuses_forged_output_without_consumption() {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&directory.path().join("forged.sqlite"));
    let (id, _) = store
        .inject_mapping_bundle(&bundle_raw("MEAS-1", "1.20"), &bundle_input())
        .expect("capture");
    let pending = store.read().pending_observations().expect("inbox");
    let mut observed: norn_kernel::Observed<Value> =
        norn_ir::runtime::decode(&pending[0].observed).expect("observed");
    let mut bundle: norn_cell::mapping_bundle::MappingBundle =
        norn_ir::runtime::decode(&observed.input).expect("bundle");
    bundle.records[0].invocations[0].input.insert(
        "purity".into(),
        Value::from_json_text("0.80").expect("decimal"),
    );
    observed.input = norn_ir::runtime::encode(&bundle).expect("bundle");
    observed.id.0 = "forged".into();
    store
        .inject(
            "forged",
            &norn_ir::runtime::encode(&observed).expect("envelope"),
        )
        .expect("ordinary caller evidence");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    assert!(matches!(
        cell.process_mapping_bundle("forged"),
        Err(CellError::MappingBundleMismatch)
    ));
    assert_eq!(
        cell.store()
            .read()
            .pending_observations()
            .expect("inbox")
            .len(),
        2
    );
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("fact")
            .is_none()
    );
    assert!(
        cell.process_mapping_bundle(&id)
            .expect("original sealed evidence")
    );
}

#[test]
fn mapping_bundle_processing_refuses_delete_and_diagnostics_without_consumption() {
    for delete in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let mut store = mapping_bundle_store(&directory.path().join("unsupported.sqlite"));
        let mut raw = bundle_raw(
            "MEAS-1",
            if delete {
                "1.20"
            } else {
                "\"invalid decimal\""
            },
        );
        if delete {
            raw.input
                .as_object_mut()
                .expect("raw object")
                .insert("kind".into(), Value::from("delete"));
        }
        let (id, _) = store
            .inject_mapping_bundle(&raw, &bundle_input())
            .expect("retained evidence");
        let before = store.read().dump().expect("before");
        let mut cell =
            norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
        assert!(matches!(
            cell.process_mapping_bundle(&id),
            Err(CellError::UnsupportedMappingProfile)
        ));
        assert_eq!(cell.store().read().dump().expect("after"), before);
        assert_eq!(
            cell.store()
                .read()
                .pending_observations()
                .expect("retained inbox")
                .len(),
            1
        );
    }
}

#[test]
fn mapping_bundle_retains_nonconforming_source_records_and_complete_diagnostics() {
    let dir = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&dir.path().join("invalid.sqlite"));
    let raw = bundle_raw("MEAS-1", "\"invalid decimal\"");
    store
        .inject_mapping_bundle(&raw, &bundle_input())
        .expect("capture malformed source value");
    let pending = store.read().pending_observations().expect("inbox");
    let observed: norn_kernel::Observed<Value> =
        norn_ir::runtime::decode(&pending[0].observed).expect("observed");
    let bundle: norn_cell::mapping_bundle::MappingBundle =
        norn_ir::runtime::decode(&observed.input).expect("bundle");
    assert_eq!(bundle.raw, raw.input);
    assert!(bundle.records.is_empty());
    assert!(!bundle.diagnostics.is_empty());
    assert!(bundle.diagnostics.iter().all(|d| d.get("code").is_some()
        && d.get("span").is_some()
        && d.get("severity").is_some()));
    store
        .read()
        .recheck_mapping_bundle(&bundle)
        .expect("same diagnostics at processing");
}

#[test]
fn mapping_bundle_refuses_mismatched_delivery_identity_without_consumption() {
    let dir = tempfile::tempdir().expect("directory");
    let mut source = mapping_bundle_store(&dir.path().join("source.sqlite"));
    let (id, _) = source
        .inject_mapping_bundle(&bundle_raw("MEAS-1", "1.20"), &bundle_input())
        .expect("bundle");
    let pending = source.read().pending_observations().expect("inbox");
    let mut observed: norn_kernel::Observed<Value> =
        norn_ir::runtime::decode(&pending[0].observed).expect("envelope");
    observed.id.0 = "other-delivery".into();
    let mut store = mapping_bundle_store(&dir.path().join("target.sqlite"));
    store
        .inject(&id, &norn_ir::runtime::encode(&observed).expect("payload"))
        .expect("untrusted stored envelope");
    store
        .inject(
            "other-delivery",
            &norn_ir::runtime::encode(&observed).expect("other payload"),
        )
        .expect("other inbox row");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    assert!(matches!(
        cell.process_mapping_bundle(&id),
        Err(CellError::MappingBundleMismatch)
    ));
    assert_eq!(
        cell.store()
            .read()
            .pending_observations()
            .expect("inbox")
            .len(),
        2
    );
    assert!(
        cell.store()
            .read()
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("fact")
            .is_none()
    );
}

#[test]
fn instance_source_provenance_rechecks_bundle_and_disclosure_for_exact_version() {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&directory.path().join("lineage.sqlite"));
    let raw = bundle_raw("MEAS-1", "1.20");
    let (id, _) = store
        .inject_mapping_bundle(&raw, &bundle_input())
        .expect("capture");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    assert!(cell.process_mapping_bundle(&id).expect("processing"));
    let reader = cell.store().read();
    let instance = reader
        .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("instance")
        .expect("captured");
    let fact = norn_ir::FactRef::new("quality.Measurement", "MEAS-1", instance.version);
    let budget = || {
        DecodeBudget::new(&ReactiveLimits {
            max_scan: 100,
            max_candidate_bytes_per_commit: 10_000_000,
            ..ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1())
        })
    };
    assert!(
        reader
            .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(false))
            .expect("withheld")
            .is_none()
    );
    let evidence = reader
        .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
        .expect("checked source lineage")
        .expect("bundle");
    assert_eq!(id, format!("mapping-bundle:{}", evidence.bundle_digest));
    assert_eq!(evidence.source_record_ref.record, "Rows/MEAS-1");
    assert_eq!(evidence.source_record_ref.system, "synthetic-erpnext");
    assert_eq!(evidence.document_ref.system, "synthetic-erpnext");
    assert_eq!(evidence.document_ref.record, "MEAS-1");
    assert_eq!(
        evidence.document_digest,
        raw.input
            .get("payload_digest")
            .and_then(Value::as_str)
            .expect("source digest")
    );
    assert_eq!(evidence.mapping_pin.artifact_id, "quality.mapping");
    assert_eq!(evidence.mapping_pin.version, "1.0.0");
    let retained = reader
        .installed_definitions(None)
        .expect("definitions")
        .expect("installed");
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_str(&retained.canonical_set).expect("artifacts");
    let mapping = artifacts
        .iter()
        .find(|a| a.artifact_id == "quality.mapping")
        .expect("mapping");
    assert_eq!(
        evidence.mapping_pin.sha256,
        norn_checker::ArtifactRef::of(mapping)
            .expect("mapping digest")
            .sha256
    );
    assert!(
        evidence
            .field_provenance
            .keys()
            .any(|p| p.as_str() == "purity")
    );
    assert!(
        reader
            .instance_source_provenance("other-world", &fact, &mut budget(), |_| panic!(
                "no disclosure of an unproven world"
            ))
            .is_err()
    );
    let unknown = norn_ir::FactRef {
        version: fact.version.checked_add(1).expect("next version"),
        ..fact
    };
    assert!(
        reader
            .instance_source_provenance(MAIN_WORLD, &unknown, &mut budget(), |_| panic!(
                "no disclosure of an unproven version"
            ))
            .is_err()
    );
}

#[test]
fn instance_source_provenance_charges_external_payloads_and_omits_non_bundle_facts() {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&directory.path().join("budget.sqlite"));
    let mut raw = bundle_raw("MEAS-1", "1.20");
    let payload = raw
        .input
        .as_object_mut()
        .expect("raw")
        .get_mut("payload")
        .expect("payload");
    payload
        .as_object_mut()
        .expect("source record")
        .insert("ddlSql".into(), Value::from("x".repeat(500_000)));
    let hash = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(payload).expect("payload JSON"))
        .expect("sealed digest");
    raw.input
        .as_object_mut()
        .expect("raw")
        .insert("payload_digest".into(), Value::from(hash));
    let (id, _) = store
        .inject_mapping_bundle(&raw, &bundle_input())
        .expect("capture");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    assert!(cell.process_mapping_bundle(&id).expect("processing"));
    let reader = cell.store().read();
    let instance = reader
        .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("read")
        .expect("instance");
    let fact = norn_ir::FactRef::new("quality.Measurement", "MEAS-1", instance.version);
    let limits = |bytes| ReactiveLimits {
        max_scan: 100,
        max_candidate_bytes_per_commit: bytes,
        ..ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1())
    };
    assert!(matches!(
        reader.instance_source_provenance(
            MAIN_WORLD,
            &fact,
            &mut DecodeBudget::new(&limits(100_000)),
            |_| Ok(true)
        ),
        Err(CellError::MappingBundleMismatch)
    ));
    assert!(
        reader
            .instance_source_provenance(
                MAIN_WORLD,
                &fact,
                &mut DecodeBudget::new(&limits(10_000_000)),
                |_| Ok(true)
            )
            .expect("full byte budget")
            .is_some()
    );
    drop(cell);
    let mut cell = purity_quality_host(&directory.path().join("ordinary.sqlite"));
    capture_purity(&mut cell, "ordinary-observation", "0.80");
    let reader = cell.store().read();
    let instance = reader
        .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("read")
        .expect("instance");
    let fact = norn_ir::FactRef::new("quality.Measurement", "MEAS-1", instance.version);
    assert!(
        reader
            .instance_source_provenance(
                MAIN_WORLD,
                &fact,
                &mut DecodeBudget::new(&limits(10_000_000)),
                |_| Ok(true)
            )
            .expect("non-bundle observation")
            .is_none()
    );
}

#[test]
fn legacy_law_snapshot_refuses_without_effects() {
    let (retained, input) = governing_law_input("self.name != 'bad'", 1);
    let mut legacy = serde_json::to_value(retained.definitions_snapshot()).expect("snapshot");
    legacy
        .as_object_mut()
        .expect("snapshot object")
        .remove("lawArtifacts");
    let snapshot = serde_json::from_value(legacy).expect("legacy snapshot decodes");
    let legacy = Model::from_snapshot(&snapshot).expect("legacy entity projection");
    assert_eq!(legacy.model_hash(), model(false).model_hash());
    let mut store = CellStore::open_in_memory(legacy.clone()).expect("store");
    let before = store.read().ledger_page(0, 100).expect("ledger");
    let refusal = store
        .install_definitions(legacy, input, Some(0), "org_demo/dev")
        .expect_err("unretained laws refuse");
    assert!(matches!(refusal, CellError::LegacyLawSnapshot));
    assert!(
        refusal
            .to_string()
            .starts_with("core.boundary.legacy_law_snapshot:")
    );
    assert_eq!(store.read().ledger_page(0, 100).expect("ledger"), before);
    assert!(
        store
            .read()
            .installed_definitions(None)
            .expect("definitions")
            .is_none()
    );
    assert!(
        store
            .read()
            .object(MAIN_WORLD, "defs.Item", "existing")
            .expect("fact")
            .is_none()
    );
}

#[test]
fn legacy_version_zero_finding_refuses_without_effects() {
    let directory = tempfile::tempdir().expect("directory");
    let admitted = purity_quality_host(&directory.path().join("admitted.sqlite"));
    let model = admitted.store().model().clone();
    let store = CellStore::open_in_memory(model).expect("unseeded store");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
    capture_purity_with_retry(&mut cell, "unseeded", "1.20", false);
    let reader = cell.store().read();
    let before = reader.ledger_page(0, 100).expect("ledger");
    let raw = reader
        .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("raw")
        .expect("record");
    let finding = before
        .iter()
        .find(|entry| entry.event.get("op").and_then(Value::as_str) == Some("data_quality_finding"))
        .expect("finding");
    assert_eq!(
        reader
            .receipt_at(finding.commit_seq)
            .expect("receipt")
            .expect("recorded")
            .body
            .definitions_version,
        0
    );
    for refusal in [
        reader
            .observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect_err("unseeded finding"),
        reader
            .observation_is_quarantined(MAIN_WORLD, &raw)
            .expect_err("unseeded quarantine"),
        reader
            .authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect_err("unseeded object"),
        reader
            .authoritative_page(MAIN_WORLD, "quality.Measurement", None, 10)
            .expect_err("unseeded page"),
        reader
            .verified_history_event(finding)
            .expect_err("no current catalog substitution"),
    ] {
        assert!(matches!(refusal, CellError::UnpinnedFinding));
        assert!(
            refusal
                .to_string()
                .starts_with("core.boundary.unpinned_finding:")
        );
    }
    assert_eq!(
        reader.ledger_page(0, 100).expect("unchanged ledger"),
        before
    );
    assert_eq!(
        reader
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .expect("unchanged fact"),
        Some(raw)
    );
    assert!(
        reader
            .installed_definitions(None)
            .expect("definitions")
            .is_none()
    );
}

// HOST-PROVISION-ATOMIC: every SQL boundary, transaction commit and lost reply.
fn provision_engine(path: &Path) -> NativeEngine {
    NativeEngine::open(
        path,
        StoreOptions {
            cache_pages: Some(1),
            ..StoreOptions::default()
        },
    )
    .expect("file engine with page spills")
}

struct ProvisionFault {
    inner: NativeEngine,
    step: std::rc::Rc<std::cell::Cell<usize>>,
    fail: usize,
    crash: bool,
}

impl ProvisionFault {
    #[expect(
        clippy::disallowed_methods,
        reason = "the crash probe deliberately exits without running transaction destructors"
    )]
    fn boundary(&self) -> Result<(), CellError> {
        let step = self.step.get().saturating_add(1);
        self.step.set(step);
        if step == self.fail {
            if self.crash {
                std::process::exit(73);
            }
            return Err(CellError::Engine("provision fault".to_owned()));
        }
        Ok(())
    }
}

impl SqlEngine for ProvisionFault {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)?;
        self.boundary()
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        let rows = self.inner.query(sql, params)?;
        self.boundary()?;
        Ok(rows)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)?;
        self.boundary()
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        let value = self.inner.transaction(|| {
            let value = f()?;
            self.boundary()?;
            Ok::<T, X>(value)
        })?;
        self.boundary()?;
        Ok(value)
    }
}

fn provision_input() -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: "[]".to_owned(),
        registry: Vec::new(),
        receipt_sha256: "synthetic".to_owned(),
        admission_receipt: None,
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: Some(
            String::from_utf8(
                norn_ir::runtime::encode(&norn_ir::LayoutSidecar::default())
                    .expect("layout")
                    .canonical_json()
                    .expect("canonical layout"),
            )
            .expect("UTF-8 layout"),
        ),
    }
}

#[test]
fn host_provision_atomic_crash_child() {
    let Ok(path) = std::env::var("NORN_PROVISION_CRASH_PATH") else {
        return;
    };
    let fail = std::env::var("NORN_PROVISION_CRASH_STEP")
        .expect("step")
        .parse()
        .expect("number");
    let engine = ProvisionFault {
        inner: provision_engine(Path::new(&path)),
        step: std::rc::Rc::new(std::cell::Cell::new(0)),
        fail,
        crash: true,
    };
    let _store =
        CellStore::provision_definitions(engine, model(false), &provision_input(), "demo/dev")
            .expect("provision");
    panic!("crash boundary was not reached");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "crash conformance requires an isolated copy of this test executable"
)]
fn host_provision_atomic_at_every_storage_boundary() {
    let engine = ProvisionFault {
        inner: NativeEngine::open_in_memory().expect("open"),
        step: std::rc::Rc::new(std::cell::Cell::new(0)),
        fail: usize::MAX,
        crash: false,
    };
    let counter = std::rc::Rc::clone(&engine.step);
    let _store =
        CellStore::provision_definitions(engine, model(false), &provision_input(), "demo/dev")
            .expect("provision");
    // Count only the public provisioning path, including its commit/lost-reply boundary.
    let steps = counter.get();
    for crash in [false, true] {
        for fail in 1..=steps {
            let dir = tempfile::tempdir().expect("dir");
            let path = dir.path().join("tenant.sqlite");
            if crash {
                let output =
                    std::process::Command::new(std::env::current_exe().expect("test binary"))
                        .args(["--exact", "host_provision_atomic_crash_child"])
                        .env("NORN_PROVISION_CRASH_PATH", &path)
                        .env("NORN_PROVISION_CRASH_STEP", fail.to_string())
                        .output()
                        .expect("crash child");
                assert_eq!(output.status.code(), Some(73), "step {fail}");
            } else {
                let engine = ProvisionFault {
                    inner: provision_engine(&path),
                    step: std::rc::Rc::new(std::cell::Cell::new(0)),
                    fail,
                    crash: false,
                };
                assert!(
                    CellStore::provision_definitions(
                        engine,
                        model(false),
                        &provision_input(),
                        "demo/dev"
                    )
                    .is_err()
                );
            }
            let engine = provision_engine(&path);
            assert_eq!(
                CellStore::engine_is_fresh(&engine).expect("fresh"),
                fail < steps,
                "crash={crash} step={fail}"
            );
            let store = CellStore::provision_definitions(
                engine,
                model(false),
                &provision_input(),
                "demo/dev",
            )
            .expect("retry");
            assert_eq!(store.read().receipts().expect("receipts").len(), 1);
            assert_eq!(
                store
                    .read()
                    .installed_definitions(None)
                    .expect("set")
                    .expect("active")
                    .version,
                1
            );
            drop(store);
            for change in 0..3 {
                let engine = provision_engine(&path);
                let mut input = provision_input();
                if change == 0 {
                    input.canonical_set = "[{}]".to_owned();
                }
                let scope = if change == 1 { "other/dev" } else { "demo/dev" };
                assert!(
                    CellStore::provision_definitions(engine, model(change == 2), &input, scope)
                        .is_err()
                );
            }
            let store = CellStore::provision_definitions(
                provision_engine(&path),
                model(false),
                &provision_input(),
                "demo/dev",
            )
            .expect("unchanged retry");
            assert_eq!(
                store.read().receipts().expect("unchanged receipts").len(),
                1
            );
        }
    }
}

#[test]
fn host_provision_atomic_retry_preserves_later_installs() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("tenant.sqlite");
    let original = provision_input();
    let mut store = CellStore::provision_definitions(
        provision_engine(&path),
        model(false),
        &original,
        "demo/dev",
    )
    .expect("first provision");
    let mut next = original.clone();
    next.installed_at = 2;
    store
        .install_definitions(model(true), next, Some(1), "demo/dev")
        .expect("later install");
    drop(store);
    let store = CellStore::provision_definitions(
        provision_engine(&path),
        model(false),
        &original,
        "demo/dev",
    )
    .expect("original retry");
    assert_eq!(store.model().model_hash(), model(true).model_hash());
    assert_eq!(store.read().receipts().expect("receipts").len(), 2);
    assert_eq!(
        store.read().active_definitions_version().expect("active"),
        Some(2)
    );
}

norn_testkit::proptest::proptest! {
    #[test]
    fn feed_conflict_replay_preserves_first_capture_and_one_finding(fraction in 1u16..1000, deliveries in 2i64..5) {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("conflict.sqlite");
        let mut store = mapping_bundle_store(&path);
        let input = bundle_input();
        let original = bundle_raw("MEAS-1", "0.90");
        let (accepted_id, _) = store.inject_mapping_bundle(&original, &input).expect("original");
        let model = store.model().clone();
        let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
        cell.process_mapping_bundle(&accepted_id).expect("apply first");
        drop(cell);
        store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("second mapping context");
        let mut alternate = input.clone(); alternate.mapping = "quality.alternate".into();
        let (second_id, _) = store.inject_mapping_bundle(&original, &alternate).expect("second role");
        let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
        cell.process_mapping_bundle(&second_id).expect("apply second role");
        let before = cell.store().read().object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("read").expect("object");
        drop(cell);
        store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
        let mut expected_id = None;
        for attempt in 0..deliveries {
            let mut raw = bundle_raw("MEAS-1", &format!("1.{fraction:03}"));
            raw.input.as_object_mut().expect("raw object").insert("extracted_at".into(), Value::from(100i64.checked_add(attempt).expect("receipt metadata")));
            let (id, inserted) = store.inject_mapping_bundle(&raw, &input).expect("conflict capture");
            norn_testkit::proptest::prop_assert_eq!(inserted, attempt == 0);
            if let Some(expected) = &expected_id { norn_testkit::proptest::prop_assert_eq!(&id, expected); }
            expected_id = Some(id.clone());
            let pending = store.read().pending_observations().expect("queue");
            norn_testkit::proptest::prop_assert_eq!(pending.len(), 1);
            let observed: norn_kernel::Observed<Value> = norn_ir::runtime::decode(&pending[0].observed).expect("envelope");
            let conflict: norn_cell::mapping_conflict::MappingConflict = norn_ir::runtime::decode(&observed.input).expect("typed finding");
            store.read().recheck_mapping_conflict(&conflict).expect("recomputed evidence");
            norn_testkit::proptest::prop_assert_eq!(&conflict.accepted_bundle_id, &accepted_id);
            norn_testkit::proptest::prop_assert_eq!(conflict.finding.status, norn_ir::DataQualityFindingStatus::Open);
            let mut forged = conflict.clone(); forged.finding.message.push('!');
            norn_testkit::proptest::prop_assert!(matches!(store.read().recheck_mapping_conflict(&forged), Err(CellError::MappingBundleMismatch)));
            let bypass = format!("mapping-bundle:{}", conflict.incoming.digest);
            if attempt.checked_add(1) == Some(deliveries) {
                let forged = norn_kernel::Observed { id: norn_kernel::ObservationId(bypass.clone()), provenance: raw.provenance.clone(), input: norn_ir::runtime::encode(&conflict.incoming).expect("bundle") };
                store.inject(&bypass, &norn_ir::runtime::encode(&forged).expect("envelope")).expect("untrusted claim");
            }
            let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
            norn_testkit::proptest::prop_assert!(matches!(cell.process_mapping_bundle(&id), Err(CellError::UnsupportedMappingProfile)));
            if attempt.checked_add(1) == Some(deliveries) {
                let delivery = norn_kernel::CellHost::observe(&mut cell).expect("queue").expect("conflict");
                norn_kernel::CellHost::commit(&mut cell, norn_kernel::CellCommit::empty().consuming(delivery.consumed())).expect("ordinary acknowledgement receipt");
                norn_testkit::proptest::prop_assert!(matches!(cell.process_mapping_bundle(&bypass), Err(CellError::UnsupportedMappingProfile)));
            }
            norn_testkit::proptest::prop_assert!(cell.store().read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_none());
            norn_testkit::proptest::prop_assert_eq!(cell.store().read().object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("first bytes"), Some(before.clone()));
            norn_testkit::proptest::prop_assert!(cell.store().read().object(MAIN_WORLD, "quality.Measurement", "MEAS-1-ALT").expect("raw alternate").is_some());
            norn_testkit::proptest::prop_assert!(cell.store().read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1-ALT").expect("alternate excluded").is_none());
            let governed = norn_kernel::FactKey::new("quality.Measurement", "MEAS-1");
            norn_testkit::proptest::prop_assert!(norn_kernel::CellHost::load(&cell, &governed).is_err());
            let page = cell.store().read().authoritative_page(MAIN_WORLD, "quality.Measurement", None, 10).expect("aggregate population");
            norn_testkit::proptest::prop_assert_eq!(page.quarantine_count, 2);
            norn_testkit::proptest::prop_assert!(page.records.is_empty());
            norn_testkit::proptest::prop_assert_eq!(cell.store().read().observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("review").len(), 1);
            drop(cell);
            store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
        }
        let mut correction = bundle_raw("MEAS-1", "0.80");
        correction.input.as_object_mut().expect("raw").insert("event_id".into(), Value::from("2"));
        let (id, _) = store.inject_mapping_bundle(&correction, &input).expect("new source event");
        let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("driver");
        cell.process_mapping_bundle(&id).expect("capture correction");
        norn_testkit::proptest::prop_assert!(cell.store().read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_none());
        norn_testkit::proptest::prop_assert_eq!(cell.store().read().object_at_version(MAIN_WORLD, "quality.Measurement", "MEAS-1", before.version).expect("immutable first version"), Some(before));

    }
}

#[test]
fn feed_conflict_uses_ingress_content_and_partition_semantics() {
    let dir = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&dir.path().join("scopes.sqlite"));
    let raw = bundle_raw("MEAS-1", "0.90");
    let input = bundle_input();
    store.inject_mapping_bundle(&raw, &input).expect("original");
    let mut metadata = raw.clone();
    metadata
        .input
        .as_object_mut()
        .expect("raw object")
        .insert("cursor".into(), Value::from("new cursor"));
    metadata
        .input
        .as_object_mut()
        .expect("raw object")
        .insert("pipeline_version".into(), Value::from("pipeline/2"));
    let (id, _) = store
        .inject_mapping_bundle(&metadata, &input)
        .expect("metadata");
    assert!(id.starts_with("mapping-bundle:"));
    for (field, value) in [
        ("source_version", Value::from("2")),
        ("kind", Value::from("delete")),
        (
            "redaction",
            norn_ir::runtime_object! {"policy_id"=>"synthetic", "policy_version"=>"1", "redacted"=>Value::Array(Vec::new())},
        ),
    ] {
        let mut changed = raw.clone();
        changed
            .input
            .as_object_mut()
            .expect("raw object")
            .insert(field.into(), value);
        let (id, _) = store
            .inject_mapping_bundle(&changed, &input)
            .expect("conflict");
        assert!(id.starts_with("mapping-conflict:"));
    }
    let mut partition = input;
    partition.partition.push('x');
    let (id, _) = store
        .inject_mapping_bundle(&bundle_raw("MEAS-1", "1.20"), &partition)
        .expect("independent partition");
    assert!(id.starts_with("mapping-bundle:"));
}

#[test]
fn source_event_lookup_matches_canonical_inbox_text_without_rewriting_rows() {
    let dir = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&dir.path().join("unicode-lookup.sqlite"));
    let mut raw = bundle_raw("MEAS-1", "0.90");
    raw.input
        .as_object_mut()
        .expect("raw")
        .insert("source_id".into(), Value::from("é"));
    raw.input
        .as_object_mut()
        .expect("raw")
        .insert("event_id".into(), Value::from("é"));
    let mut input = bundle_input();
    input.partition = "é".into();
    store.inject_mapping_bundle(&raw, &input).expect("capture");
    let found = store
        .read()
        .source_event_mapping_bundles(
            "demo",
            "e\u{301}",
            "synthetic-erpnext",
            "e\u{301}",
            "e\u{301}",
        )
        .expect("canonical lookup");
    assert_eq!(found.len(), 1);
    assert_eq!(
        store
            .read()
            .retained_mapping_root(found[0].definitions_version, "quality.Measurement")
            .expect("retained root"),
        "quality.Measurement"
    );
}

// OBS-EVIDENCE-1: NFC lookup equivalence cannot merge distinct source spelling bytes.
norn_testkit::proptest::proptest! {
    #[test]
    fn identity_normalized_redelivery_is_one_durable_finding(prefix in "[a-z]{0,5}", first_nfc in norn_testkit::proptest::bool::ANY, deliveries in 2i64..4) {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("identity.sqlite");
        let mut store = mapping_bundle_store(&path);
        let input = bundle_input();
        let mut canonical = bundle_raw("MEAS-1", "0.90");
        canonical.input.as_object_mut().expect("raw").insert("source_id".into(), Value::from(format!("{prefix}é")));
        let mut decomposed = canonical.clone();
        decomposed.input.as_object_mut().expect("raw").insert("source_id".into(), Value::from(format!("{prefix}e\u{301}")));
        let model = store.model().clone();
        if first_nfc {
            let (id, _) = store.inject_mapping_bundle(&canonical, &input).expect("first accepted");
            let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
            cell.process_mapping_bundle(&id).expect("apply first");
            drop(cell);
            store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
        }
        let expected = norn_kernel::FactVersion(store.read().version(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("read before dispute"));
        let (guard_id, inserted) = store.inject_mapping_bundle(&decomposed, &input).expect("capture spelling");
        norn_testkit::proptest::prop_assert!(inserted && guard_id.starts_with("mapping-identity:"));
        let (canonical_id, _) = store.inject_mapping_bundle(&canonical, &input).expect("candidate retained");
        let mut cell = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
        norn_testkit::proptest::prop_assert!(matches!(cell.process_mapping_bundle(&guard_id), Err(CellError::UnsupportedMappingProfile)));
        if !first_nfc {
            norn_testkit::proptest::prop_assert!(matches!(cell.process_mapping_bundle(&canonical_id), Err(CellError::UnsupportedMappingProfile)));
        }
        let key = norn_kernel::FactKey::new("quality.Measurement", "MEAS-1");
        norn_testkit::proptest::prop_assert!(norn_kernel::CellHost::load(&cell, &key).is_err());
        norn_testkit::proptest::prop_assert!(norn_kernel::CellHost::load_bounded(&cell, &key, 4096).is_err());
        norn_testkit::proptest::prop_assert!(norn_kernel::CellHost::load_reference(&cell, &key, 4096).is_err());
        let link = norn_kernel::LinkKey::new(norn_kernel::FactKey::new("quality.Measurement", "MEAS-2"), "unused", key.clone());
        norn_testkit::proptest::prop_assert!(matches!(norn_kernel::CellHost::load_link(&cell, &link), Err(CellError::SourceIdentityDisputed)));
        norn_testkit::proptest::prop_assert!(matches!(norn_kernel::CellHost::read_link_targets(&cell, &key, "unused", 1, 64), Err(CellError::SourceIdentityDisputed)));
        let mut stale = norn_kernel::CellCommit::empty();
        stale.reads.push(norn_kernel::FactExpectation { key: key.clone(), expected });
        norn_testkit::proptest::prop_assert!(matches!(norn_kernel::CellHost::commit(&mut cell, stale), Err(norn_kernel::CommitError::Host(CellError::SourceIdentityDisputed))));
        let mut written = norn_kernel::CellCommit::empty();
        written.writes.push(norn_kernel::FactWrite { key, expected, mutation: norn_kernel::FactMutation::Put { values:BTreeMap::from([("purity".into(), Value::from_json_text("0.90").expect("value"))]) } });
        norn_testkit::proptest::prop_assert!(matches!(norn_kernel::CellHost::commit(&mut cell, written), Err(norn_kernel::CommitError::Host(CellError::SourceIdentityDisputed))));
        drop(cell);
        for delivery in 1..deliveries {
            store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("restart");
            let mut replay = decomposed.clone();
            replay.id.0 = format!("delivery-{delivery}");
            replay.input.as_object_mut().expect("raw").insert("extracted_at".into(), Value::from(1_793_527_260_000i64.checked_add(delivery).expect("bounded time")));
            let (id, inserted) = store.inject_mapping_bundle(&replay, &input).expect("redelivery");
            norn_testkit::proptest::prop_assert_eq!(&id, &guard_id);
            norn_testkit::proptest::prop_assert!(!inserted);
            let findings = store.read().observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("durable finding");
            norn_testkit::proptest::prop_assert_eq!(findings.len(), 1);
            norn_testkit::proptest::prop_assert_eq!(&findings[0].message, "identity normalized; possible collision");
            norn_testkit::proptest::prop_assert!(store.read().authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("projection").is_none());
            if first_nfc {
                let page = store.read().authoritative_page(MAIN_WORLD, "quality.Measurement", None, 10).expect("aggregate population");
                norn_testkit::proptest::prop_assert_eq!(page.quarantine_count, 1);
                norn_testkit::proptest::prop_assert!(page.records.is_empty());
                norn_testkit::proptest::prop_assert!(store.read().object(MAIN_WORLD, "quality.Measurement", "MEAS-1").expect("first preserved").is_some());
            }
        }
    }
}

#[test]
fn identity_guard_keeps_spelling_bytes_and_refuses_an_altered_finding() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("guard.sqlite");
    let mut store = mapping_bundle_store(&path);
    let mut input = bundle_input();
    input.partition = "e\u{301}".into();
    let (id, _) = store
        .inject_mapping_bundle(&bundle_raw("MEAS-1", "0.90"), &input)
        .expect("guard");
    let pending = store.read().pending_observations().expect("inbox");
    assert_eq!(pending.len(), 1);
    let captured: norn_kernel::Observed<Value> =
        norn_ir::runtime::decode(&pending[0].observed).expect("guard envelope");
    let proof = serde_json::to_value(&captured.input).expect("JSON proof");
    assert_eq!(proof["identity"][1], json!([101, 204, 129]));
    assert_eq!(proof["bundle"]["input"]["partition"], "é");
    let mut altered = captured;
    altered.id.0.push_str("-forged");
    altered
        .input
        .as_object_mut()
        .expect("input")
        .get_mut("finding")
        .expect("finding")
        .as_object_mut()
        .expect("object")
        .insert("status".into(), Value::from("resolved"));
    store
        .inject(
            &altered.id.0,
            &norn_ir::runtime::encode(&altered).expect("encode"),
        )
        .expect("untrusted inbox evidence");
    assert!(matches!(
        store
            .read()
            .observation_findings(MAIN_WORLD, "quality.Measurement", "MEAS-1"),
        Err(CellError::MappingBundleMismatch)
    ));
    assert!(id.starts_with("mapping-identity:"));
}

#[test]
fn retained_mapping_root_survives_removal_of_an_unapplied_type() {
    let dir = tempfile::tempdir().expect("directory");
    let mut store = mapping_bundle_store(&dir.path().join("retained-root.sqlite"));
    let previous = store
        .read()
        .installed_definitions(None)
        .expect("definitions")
        .expect("installed");
    let model = model_from_json(vec![item_type(vec![
        json!({"id":"name", "type":{"kind":"text"}}),
    ])]);
    let installation = install_input(
        "without-quality",
        previous.installed_at.checked_add(1).expect("epoch"),
        &model,
    );
    store
        .install_definitions(model, installation, Some(previous.version), "org_demo/dev")
        .expect("remove empty type");
    assert!(
        store
            .read()
            .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
            .is_err()
    );
    assert_eq!(
        store
            .read()
            .retained_mapping_root(previous.version, "quality.Measurement")
            .expect("retained subject"),
        "quality.Measurement"
    );
}

fn source_dispute_world_fixture(
    normalized: bool,
) -> (
    tempfile::TempDir,
    CellStore,
    norn_cell::ObjectRecord,
    norn_cell::ObjectRecord,
) {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("world-dispute.sqlite");
    let mut store = mapping_bundle_store(&path);
    let input = bundle_input();
    let mut original = bundle_raw("MEAS-1", "0.90");
    original
        .input
        .as_object_mut()
        .expect("raw")
        .insert("source_id".into(), Value::from("é"));
    let (id, _) = store
        .inject_mapping_bundle(&original, &input)
        .expect("capture");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    cell.process_mapping_bundle(&id).expect("apply first");
    let before = cell
        .store()
        .read()
        .authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
        .expect("eligible")
        .expect("record");
    let model = cell.store().model().clone();
    drop(cell);
    store = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
    store
        .commit(&CommitBatch {
            world: "fork/test".into(),
            scope: "org_demo/dev".into(),
            logical_time: store.read().clock_floor().expect("clock"),
            writes: vec![FactWriteRow::Put {
                object_type: "quality.Measurement".into(),
                key: "MEAS-1".into(),
                values: before.values.clone(),
            }],
            ..CommitBatch::default()
        })
        .expect("synthetic non-main fixture");
    let fork = store
        .read()
        .object("fork/test", "quality.Measurement", "MEAS-1")
        .expect("raw world")
        .expect("record");
    let mut candidate = if normalized {
        original.clone()
    } else {
        bundle_raw("MEAS-1", "1.20")
    };
    candidate.input.as_object_mut().expect("raw").insert(
        "source_id".into(),
        Value::from(if normalized { "e\u{301}" } else { "é" }),
    );
    store
        .inject_mapping_bundle(&candidate, &input)
        .expect("durable dispute");
    (dir, store, before, fork)
}

#[test]
fn source_dispute_non_main_reads_refuse_until_core08_retains_the_frontier() {
    for normalized in [false, true] {
        let (_directory, store, before, fork) = source_dispute_world_fixture(normalized);
        let reader = store.read();
        let expected = "core.world.source_dispute_unqualified";
        assert_eq!(
            reader
                .authoritative_object("fork/test", "quality.Measurement", "MEAS-1")
                .expect_err("object refuses")
                .to_string(),
            expected
        );
        assert_eq!(
            reader
                .authoritative_page("fork/test", "quality.Measurement", None, 10)
                .expect_err("page refuses")
                .to_string(),
            expected
        );
        assert_eq!(
            reader
                .observation_findings("fork/test", "quality.Measurement", "MEAS-1")
                .expect_err("findings refuse")
                .to_string(),
            expected
        );
        assert_eq!(
            reader
                .observation_is_quarantined("fork/test", &fork)
                .expect_err("governed predicate refuses")
                .to_string(),
            expected
        );
        assert!(
            reader
                .authoritative_object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
                .expect("live quarantine")
                .is_none()
        );
        assert_eq!(
            reader
                .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
                .expect("raw preserved"),
            Some(before.clone())
        );
        assert_eq!(
            reader
                .object("fork/test", "quality.Measurement", "MEAS-1")
                .expect("raw world preserved"),
            Some(fork)
        );
        assert!(
            reader
                .authoritative_object("fork/test", "quality.Measurement", "OTHER")
                .expect("unaffected absence")
                .is_none()
        );
        let mut cell: norn_cell::SqliteCell<
            norn_kernel::ObservationEvent,
            norn_ir::RuntimeObject,
            norn_ir::RuntimeObject,
        > = norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new())
            .expect("native cell");
        let mut written = norn_kernel::CellCommit::empty();
        written.writes.push(norn_kernel::FactWrite {
            key: norn_kernel::FactKey::new("quality.Measurement", "MEAS-1"),
            expected: norn_kernel::FactVersion(before.version),
            mutation: norn_kernel::FactMutation::Put {
                values: before.values,
            },
        });
        assert!(matches!(
            norn_kernel::CellHost::commit(&mut cell, written),
            Err(norn_kernel::CommitError::Host(
                CellError::SourceIdentityDisputed
            ))
        ));
    }
}

#[test]
fn source_dispute_index_reuses_verified_snapshot_and_refuses_external_tampering() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("indexed-disputes.sqlite");
    let mut store = mapping_bundle_store(&path);
    let input = bundle_input();
    let (id, _) = store
        .inject_mapping_bundle(&bundle_raw("MEAS-1", "0.90"), &input)
        .expect("capture");
    let mut cell =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    cell.process_mapping_bundle(&id).expect("canonical fact");
    let model = cell.store().model().clone();
    drop(cell);
    store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopen");
    for number in 0..16 {
        let name = format!("MEAS-{number}");
        store
            .inject_mapping_bundle(&bundle_raw(&name, "0.90"), &input)
            .expect("first");
        store
            .inject_mapping_bundle(&bundle_raw(&name, "1.20"), &input)
            .expect("conflict");
    }
    drop(store);
    let counter = WriteCounter::default();
    let engine = CountedEngine {
        inner: NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        counter: counter.clone(),
    };
    let store = CellStore::from_engine(engine, model).expect("counted store");
    let cell: norn_cell::SqliteCell<Value, Value, norn_ir::RuntimeObject, CountedEngine> =
        norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("host");
    let clean = norn_kernel::FactKey::new("quality.Measurement", "unrelated");
    let disputed = norn_kernel::FactKey::new("quality.Measurement", "MEAS-1");
    counter.evidence_reads.set(0);
    assert!(
        norn_kernel::CellHost::load_reference(&cell, &disputed, 0)
            .expect("budget first")
            .is_none()
    );
    assert_eq!(counter.evidence_reads.get(), 0);
    norn_kernel::CellHost::load(&cell, &clean).expect("warm complete index");
    assert_eq!(
        counter.evidence_reads.get(),
        2,
        "one conflict and one identity scan"
    );
    for _ in 0..32 {
        norn_kernel::CellHost::load(&cell, &clean).expect("negative indexed lookup");
        assert!(matches!(
            norn_kernel::CellHost::load(&cell, &disputed),
            Err(CellError::SourceIdentityDisputed)
        ));
    }
    assert_eq!(
        counter.evidence_reads.get(),
        2,
        "no unrelated evidence is decoded per operand"
    );
    assert_interface_dispute_fences(&cell, &clean);
    let external = NativeEngine::open(&path, StoreOptions::default()).expect("external connection");
    external.execute("UPDATE _norn_inbox SET observed = json_set(observed, '$.input.finding.message', 'forged') WHERE observation_id LIKE 'mapping-conflict:%'", &[]).expect("synthetic corruption");
    assert!(
        norn_kernel::CellHost::load_bounded(&cell, &disputed, 0)
            .expect("budget still first")
            .is_none()
    );
    assert_eq!(counter.evidence_reads.get(), 2);
    assert!(matches!(
        norn_kernel::CellHost::load(&cell, &clean),
        Err(CellError::MappingBundleMismatch)
    ));
    assert!(
        counter.evidence_reads.get() > 2,
        "external edit invalidates negative membership too"
    );
}

// OBS-DISPUTE-LOOKUP-1: a later mapping context extends a cached source dispute.
norn_testkit::proptest::proptest! {
    #![proptest_config(norn_testkit::proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn source_dispute_index_refreshes_later_contexts_and_redelivery(prefix in "[A-Z]{1,5}", deliveries in 1usize..4) {
        let dir = tempfile::tempdir().expect("directory");
        let mut store = mapping_bundle_store(&dir.path().join("late-context.sqlite"));
        let input = bundle_input();
        let original = bundle_raw(&prefix, "0.90");
        store.inject_mapping_bundle(&original, &input).expect("accepted");
        store.inject_mapping_bundle(&bundle_raw(&prefix, "1.20"), &input).expect("conflict");
        let alternate_key = format!("{prefix}-ALT");
        norn_testkit::proptest::prop_assert!(store.read().observation_findings(MAIN_WORLD, "quality.Measurement", &alternate_key).expect("warm negative").is_empty());
        let mut alternate = input.clone(); alternate.mapping = "quality.alternate".into();
        for _ in 0..deliveries {
            store.inject_mapping_bundle(&original, &alternate).expect("later context or replay");
            let findings = store.read().observation_findings(MAIN_WORLD, "quality.Measurement", &alternate_key).expect("refreshed membership");
            norn_testkit::proptest::prop_assert_eq!(findings.len(), 1);
        }
    }
}

#[test]
fn source_dispute_index_revision_is_unavailable_for_rolled_back_state() {
    let engine = NativeEngine::open_in_memory().expect("engine");
    engine
        .execute("CREATE TABLE cache_test (value INTEGER)", &[])
        .expect("table");
    let before = engine
        .read_cache_revision()
        .expect("committed revision")
        .expect("cacheable");
    let rolled_back: Result<(), CellError> = engine.transaction(|| {
        engine.execute("INSERT INTO cache_test VALUES (1)", &[])?;
        assert_eq!(
            engine.read_cache_revision()?,
            None,
            "uncommitted state cannot seed a persistent cache"
        );
        Err(CellError::Engine("intentional rollback".into()))
    });
    assert!(rolled_back.is_err());
    let after = engine
        .read_cache_revision()
        .expect("committed revision")
        .expect("cacheable again");
    assert_ne!(before, after);
    let count: i64 = engine
        .query_row("SELECT COUNT(*) FROM cache_test", &[], |row| row.get(0))
        .expect("rollback contents");
    assert_eq!(count, 0);
}

#[test]
fn source_dispute_index_retries_churn_and_returns_a_distinct_retryable_refusal() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("revision-churn.sqlite");
    let store = mapping_bundle_store(&path);
    let model = store.model().clone();
    drop(store);
    let counter = WriteCounter::default();
    let engine = CountedEngine {
        inner: NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        counter: counter.clone(),
    };
    let store = CellStore::from_engine(engine, model).expect("store");
    for churn in 1..=8 {
        counter.revision_bumps.set(churn);
        counter.evidence_reads.set(0);
        let result = store
            .read()
            .observation_findings(MAIN_WORLD, "quality.Measurement", "clean");
        if churn < 7 {
            assert!(result.expect("bounded retry stabilizes").is_empty());
        } else {
            assert!(matches!(result, Err(CellError::SourceEvidenceChanged)));
        }
        assert!(
            counter.evidence_reads.get() <= 6,
            "at most three complete rebuild attempts"
        );
    }
}

fn assert_interface_dispute_fences(
    cell: &norn_cell::SqliteCell<Value, Value, norn_ir::RuntimeObject, CountedEngine>,
    clean: &norn_kernel::FactKey,
) {
    for target in ["MEAS-1", "MEAS-0"] {
        let link = norn_kernel::LinkKey::new(
            clean.clone(),
            "unused",
            norn_kernel::FactKey::new("quality.Measured", target),
        );
        assert!(
            matches!(
                norn_kernel::CellHost::load_link(cell, &link),
                Err(CellError::SourceIdentityDisputed)
            ),
            "both live and missing interface roots retain quarantine"
        );
    }
    let link = norn_kernel::LinkKey::new(
        clean.clone(),
        "unused",
        norn_kernel::FactKey::new("quality.Measured", "clean-interface"),
    );
    assert!(
        matches!(
            norn_kernel::CellHost::load_link(cell, &link),
            Err(CellError::UnknownLink { .. })
        ),
        "clean interfaces reach ordinary link validation"
    );
}

#[test]
fn mapping_dispute_arriving_before_commit_refuses_without_consumption() {
    for identity in [false, true] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("arrival.sqlite");
        let mut store = mapping_bundle_store(&path);
        let input = bundle_input();
        let mut original = bundle_raw("MEAS-1", "0.90");
        if identity {
            original
                .input
                .as_object_mut()
                .expect("raw")
                .insert("source_id".into(), Value::from("é"));
        }
        let (id, _) = store
            .inject_mapping_bundle(&original, &input)
            .expect("accepted");
        let model = store.model().clone();
        drop(store);
        let mut external =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("external");
        let counter = WriteCounter::default();
        let engine = CountedEngine {
            inner: NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
            counter: counter.clone(),
        };
        let store = CellStore::from_engine(engine, model).expect("store");
        let mut cell =
            norn_cell::SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
        let mut conflicting = if identity {
            original
        } else {
            bundle_raw("MEAS-1", "1.20")
        };
        if identity {
            conflicting
                .input
                .as_object_mut()
                .expect("raw")
                .insert("source_id".into(), Value::from("e\u{301}"));
        }
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let signal = std::rc::Rc::clone(&fired);
        *counter.before_transaction.borrow_mut() = Some(Box::new(move || {
            external.inject_mapping_bundle(&conflicting, &input)?;
            signal.set(true);
            Ok(())
        }));
        let result = cell.process_mapping_bundle(&id);
        assert!(
            fired.get(),
            "the dispute arrives after preparation, before BEGIN"
        );
        assert!(
            matches!(&result, Err(CellError::ObservationCommit(error)) if matches!(error.as_ref(), norn_kernel::CommitError::Host(CellError::UnsupportedMappingProfile))),
            "identity={identity}; result={result:?}"
        );
        assert!(
            cell.store()
                .read()
                .object(MAIN_WORLD, "quality.Measurement", "MEAS-1")
                .expect("raw object")
                .is_none()
        );
        assert!(
            cell.store()
                .read()
                .pending_observations()
                .expect("inbox")
                .iter()
                .any(|row| row.observation_id == id)
        );
        let mut correction = bundle_raw("MEAS-1", "0.95");
        let raw = correction.input.as_object_mut().expect("raw");
        raw.insert("event_id".into(), Value::from("next-event"));
        if identity {
            raw.insert("source_id".into(), Value::from("é"));
        }
        let mut capture =
            CellStore::open(&path, cell.store().model().clone(), StoreOptions::default())
                .expect("correction connection");
        let (correction_id, _) = capture
            .inject_mapping_bundle(&correction, &bundle_input())
            .expect("new source event");
        assert!(
            cell.process_mapping_bundle(&correction_id)
                .expect("capture a correction under a different event")
        );
    }
}
