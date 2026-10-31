#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The definition catalog projection (norn#62): every relation is projected
//! from the admitted IR it names, each reference pinned to an exact artifact
//! and member, and the projection is deterministic.

use norn_checker::definition_catalog::project;
use norn_checker::resolve_artifact_set;
use norn_ir::ArtifactEnvelope;
use norn_ir::definition_catalog::{
    ArtifactKey, DefinitionCatalog, DependencyVia, DependsOnRow, LawBinding, MapRule, Member,
    MemberRef, TypeKind, WriteVia,
};
use norn_ir::mapping::GapReason;
use serde_json::{Value, json};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn read_json(path: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!("{ROOT}/{path}"))
            .unwrap_or_else(|error| panic!("{path} should be readable: {error}")),
    )
    .unwrap_or_else(|error| panic!("{path} should parse: {error}"))
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("envelope decodes")
}

fn catalog(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> DefinitionCatalog {
    let set = resolve_artifact_set(artifacts, base).expect("set resolves");
    project(&set, base).expect("catalog projects")
}

fn sales_order() -> ArtifactEnvelope {
    envelope(read_json(
        "registry/core/artifacts/commerce.SalesOrder.entity.json",
    ))
}

fn submit_invoice() -> ArtifactEnvelope {
    envelope(read_json(
        "fixtures/ir/commerce-sales-order-submit-invoice.action.json",
    ))
}

fn order_member(member: Member) -> MemberRef {
    MemberRef {
        artifact_id: "commerce.SalesOrder.entity".to_owned(),
        version: "0.1.0".to_owned(),
        member,
    }
}

fn invoice_transition() -> MemberRef {
    order_member(Member::Transition {
        type_name: "commerce.SalesOrder".to_owned(),
        lifecycle: "status".to_owned(),
        transition: "invoice".to_owned(),
    })
}

#[test]
fn types_properties_links_and_transitions_bind_their_declaring_artifact() {
    let catalog = catalog(&[sales_order()], &[]);

    let artifact = catalog
        .artifact("commerce.SalesOrder.entity", "0.1.0")
        .expect("the artifact is pinned");
    assert!(artifact.candidate);
    assert_eq!(artifact.sha256.len(), 64);
    assert!(artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let order_type = &catalog.types[0];
    assert_eq!(
        order_type.at,
        order_member(Member::Type {
            type_name: "commerce.SalesOrder".to_owned()
        })
    );
    assert_eq!(order_type.kind, TypeKind::ObjectType);
    assert_eq!(order_type.implements, ["core.Order"]);

    let status = catalog
        .properties
        .iter()
        .find(|row| row.at.member.id() == "property:commerce.SalesOrder#status")
        .expect("status is a property row");
    let machine = status.lifecycle.as_ref().expect("status owns a lifecycle");
    assert_eq!(
        machine.states,
        [
            "draft",
            "submitted",
            "on_hold",
            "approved",
            "invoiced",
            "completed",
            "cancelled"
        ]
    );
    assert_eq!(machine.initial, ["draft"]);
    assert!(!status.computed);
    assert!(catalog.contains(&order_member(Member::State {
        type_name: "commerce.SalesOrder".to_owned(),
        lifecycle: "status".to_owned(),
        state: "invoiced".to_owned(),
    })));
    assert!(!catalog.contains(&order_member(Member::State {
        type_name: "commerce.SalesOrder".to_owned(),
        lifecycle: "status".to_owned(),
        state: "shipped".to_owned(),
    })));

    let invoice = catalog
        .transitions
        .iter()
        .find(|row| row.at == invoice_transition())
        .expect("invoice is a transition row");
    assert_eq!(invoice.from, ["approved"]);
    assert_eq!(invoice.to, "invoiced");
    assert_eq!(catalog.transitions.len(), 7);

    assert!(
        catalog
            .links
            .iter()
            .any(|row| row.at.member.id() == "link:commerce.SalesOrder#lines"
                && row.target == "commerce.SalesOrderLine")
    );
}

#[test]
fn an_action_effect_is_an_authored_write_of_the_pinned_transition() {
    let catalog = catalog(&[sales_order(), submit_invoice()], &[]);
    let writes: Vec<_> = catalog
        .writes
        .iter()
        .map(|row| (row.writer.artifact_id.as_str(), &row.target, row.via))
        .collect();
    assert_eq!(
        writes,
        [(
            "commerce.sales_order.submit_invoice.action",
            &invoice_transition(),
            WriteVia::FireTransition
        )]
    );
    assert!(catalog.contains(&catalog.writes[0].target));
}

#[test]
fn a_decide_graph_writes_only_the_declared_members_its_schema_admits() {
    let order = envelope(read_json(
        "fixtures/ir/entity-v2/artifact-set/commerce-purchase-order.entity.json",
    ));
    let rule = envelope(read_json(
        "fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json",
    ));
    let catalog = catalog(&[order, rule], &[]);
    let mut writes: Vec<(String, WriteVia)> = catalog
        .writes
        .iter()
        .map(|row| (row.target.member.id(), row.via))
        .collect();
    writes.sort();
    // `create` writes the object; `transition` writes `submit` and `approve`
    // of `status`, the only lifecycle the schema names.
    assert_eq!(
        writes,
        [
            (
                "transition:commerce.PurchaseOrder#status/approve".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "transition:commerce.PurchaseOrder#status/submit".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "type:commerce.PurchaseOrder".to_owned(),
                WriteVia::FactObject
            ),
        ]
    );
}

#[test]
fn dependencies_resolve_to_exact_versions_including_the_base() {
    let mut action = read_json("fixtures/ir/commerce-sales-order-submit-invoice.action.json");
    action["dependencies"] = json!([{
        "artifactId": "commerce.SalesOrder.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    let catalog = catalog(&[envelope(action)], &[sales_order()]);

    assert_eq!(
        catalog.depends_on,
        [DependsOnRow {
            from: ArtifactKey {
                artifact_id: "commerce.sales_order.submit_invoice.action".to_owned(),
                version: "0.1.0".to_owned(),
            },
            to: ArtifactKey {
                artifact_id: "commerce.SalesOrder.entity".to_owned(),
                version: "0.1.0".to_owned(),
            },
            via: DependencyVia::Dependency,
        }]
    );
    let base = catalog
        .artifact("commerce.SalesOrder.entity", "0.1.0")
        .expect("the referenced base artifact is pinned");
    assert!(!base.candidate);
    // Base definitions are projected too, so rules can reach them.
    assert!(catalog.contains(&invoice_transition()));
}

#[test]
fn a_mapping_observes_members_and_declares_gaps() {
    let mapping = envelope(json!({
        "irVersion": "0.1",
        "artifactId": "erpnext.sales_order.mapping",
        "kind": "mapping",
        "namespace": "erpnext",
        "name": "Sales orders",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "format": "norn.mapping/1",
            "source": {
                "schemaId": "erpnext.schema",
                "version": "0.1.0",
                "sha256": "a".repeat(64)
            },
            "interfaces": ["core.Order.interface"],
            "maps": [{
                "id": "sales_order",
                "source": { "dataset": "Sales Order" },
                "target": {
                    "entity": "commerce.SalesOrder.entity",
                    "interface": "core.Order.interface"
                },
                "subject": "input.name",
                "sourceRecord": "input.name",
                "properties": [{ "property": "total_amount", "value": "input.grand_total" }],
                "links": [{ "link": "counterparty", "key": "input.customer" }],
                "lifecycles": [{ "property": "status", "state": "input.status" }],
                "gaps": [{ "member": "amended_from", "reason": "deferred" }]
            }]
        }
    }));
    let catalog = catalog(&[sales_order(), mapping], &[]);
    let mut rows: Vec<(String, MapRule)> = catalog
        .mappings
        .iter()
        .map(|row| {
            assert_eq!(row.map.member.id(), "map:sales_order");
            assert_eq!(row.dataset, "Sales Order");
            assert_eq!(row.target.artifact_id, "commerce.SalesOrder.entity");
            (row.target.member.id(), row.rule.clone())
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            (
                "link:commerce.SalesOrder#amended_from".to_owned(),
                MapRule::Gap {
                    reason: GapReason::Deferred
                }
            ),
            (
                "link:commerce.SalesOrder#counterparty".to_owned(),
                MapRule::Link {}
            ),
            (
                "property:commerce.SalesOrder#status".to_owned(),
                MapRule::Lifecycle {}
            ),
            (
                "property:commerce.SalesOrder#total_amount".to_owned(),
                MapRule::Property {}
            ),
            ("type:commerce.SalesOrder".to_owned(), MapRule::Object {}),
        ]
    );
    // Observation is not authorship: a mapping adds no write.
    assert!(catalog.writes.is_empty());
}

#[test]
fn laws_bind_their_subject_and_response() {
    let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
    for path in ["fixtures/law-ir/entities.json", "fixtures/law-ir/laws.json"] {
        let Value::Array(values) = read_json(path) else {
            panic!("{path} is a list of artifacts");
        };
        artifacts.extend(values.into_iter().map(envelope));
    }
    let catalog = catalog(&artifacts, &[]);
    assert_eq!(catalog.laws.len(), 5);
    let identity = catalog
        .laws
        .iter()
        .find(|row| row.at.artifact_id == "erpnext.invariant_identity.law")
        .expect("the invariant law is a row");
    let purchase_order = MemberRef {
        artifact_id: "erpnext.PurchaseOrder.entity".to_owned(),
        version: "0.1.0".to_owned(),
        member: Member::Type {
            type_name: "erpnext.PurchaseOrder".to_owned(),
        },
    };
    assert_eq!(identity.subject, purchase_order);
    assert!(catalog.contains(&identity.subject));
    assert_eq!(
        identity.binds,
        Some(LawBinding::Invariant {
            owner: purchase_order,
            invariant: "amount_frozen".to_owned(),
        })
    );
    assert!(
        catalog
            .laws
            .iter()
            .filter(|row| row.at.artifact_id != "erpnext.invariant_identity.law")
            .all(|row| row.binds.is_none())
    );
}

#[test]
fn the_projection_is_deterministic_and_order_independent() {
    let forward = catalog(&[sales_order(), submit_invoice()], &[]);
    let reverse = catalog(&[submit_invoice(), sales_order()], &[]);
    assert_eq!(forward, reverse);
    let bytes = serde_json::to_string(&forward).expect("catalog serializes");
    assert!(bytes.starts_with(r#"{"format":"norn.catalog/1","#));
    let round_trip: DefinitionCatalog = serde_json::from_str(&bytes).expect("catalog decodes");
    assert_eq!(round_trip, forward);
}

fn entity(id: &str, payload: &Value) -> ArtifactEnvelope {
    envelope(json!({
        "irVersion": "0.1", "artifactId": id, "kind": "entity", "namespace": "shop",
        "name": id, "version": "1.0.0", "lifecycle": "draft", "scope": {},
        "dependencies": [], "provenance": { "source": "system" }, "payload": payload
    }))
}

fn toy_mapping(maps: &Value) -> ArtifactEnvelope {
    envelope(json!({
        "irVersion": "0.1", "artifactId": "shop.orders.mapping", "kind": "mapping",
        "namespace": "shop", "name": "orders", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "format": "norn.mapping/1",
            "source": { "schemaId": "shop.source", "version": "1.0.0", "sha256": "a".repeat(64) },
            "interfaces": ["shop.Order.entity"],
            "maps": maps
        }
    }))
}

#[test]
fn extension_members_bind_to_the_extension_that_declares_them() {
    let order = entity(
        "shop.Order.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.Order", "properties": [{ "id": "id", "type": { "kind": "text" } }]
        }}),
    );
    let extension = entity(
        "acme.Order.extension",
        &json!({ "entityVersion": "2.0", "objectTypeExtension": {
            "namespace": "acme", "target": "shop.Order",
            "properties": [{ "id": "acme_tier", "type": { "kind": "text" } }]
        }}),
    );
    let mapping = toy_mapping(&json!([{
        "id": "order", "source": { "dataset": "orders" },
        "target": { "entity": "shop.Order.entity", "interface": "shop.Order.entity" },
        "subject": "input.id", "sourceRecord": "input.id",
        "properties": [{ "property": "acme_tier", "value": "input.tier" }]
    }]));
    let catalog = catalog(&[order, extension, mapping], &[]);
    let tier = catalog
        .mappings
        .iter()
        .find(|row| row.rule == MapRule::Property {})
        .expect("the extension property is mapped");
    assert_eq!(tier.target.artifact_id, "acme.Order.extension");
    assert!(catalog.contains(&tier.target));
}

#[test]
fn a_map_with_no_member_rules_is_still_a_member() {
    let order = entity(
        "shop.Order.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.Order", "properties": [{ "id": "id", "type": { "kind": "text" } }]
        }}),
    );
    let mapping = toy_mapping(&json!([{
        "id": "order", "source": { "dataset": "orders" },
        "target": { "entity": "shop.Order.entity", "interface": "shop.Order.entity" },
        "subject": "input.id", "sourceRecord": "input.id"
    }]));
    let catalog = catalog(&[order, mapping], &[]);
    assert!(catalog.contains(&MemberRef {
        artifact_id: "shop.orders.mapping".to_owned(),
        version: "1.0.0".to_owned(),
        member: Member::ObjectMap {
            map: "order".to_owned()
        },
    }));
}

#[test]
fn a_parent_row_mutation_writes_its_child_types() {
    let parent = entity(
        "shop.Order.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.Order",
            "properties": [{ "id": "id", "type": { "kind": "text" } }],
            "links": [{ "id": "lines", "target": "shop.OrderLine",
                "cardinality": { "to": "many", "from": "one" } }],
            "ownedCollections": [{ "id": "lines", "childType": "shop.OrderLine",
                "rowIdProperty": "row_id", "orderingKey": "idx",
                "cascadeOnParentDelete": true, "maxRows": 20 }]
        }}),
    );
    let child = entity(
        "shop.OrderLine.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.OrderLine",
            "properties": [
                { "id": "row_id", "type": { "kind": "text" } },
                { "id": "idx", "type": { "kind": "integer" } }
            ]
        }}),
    );
    let action = envelope(json!({
        "irVersion": "0.1", "artifactId": "shop.edit_order.action", "kind": "action",
        "namespace": "shop", "name": "edit", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.edit_order", "scopeRequired": [], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "effectProgram": {
                "kind": "mutate_parent_with_rows", "objectIdField": "id",
                "objectKind": "shop.Order", "entityArtifactId": "shop.Order.entity",
                "entityArtifactVersion": "1.0.0", "parentValuesField": "values",
                "rowOpsField": "rows", "parentVersionField": "version",
                "collections": [{ "id": "lines" }]
            }
        }
    }));
    // The child type is installed and reached only through the owned
    // collection; it is projected and pinned like any other reference.
    let catalog = catalog(&[parent, action], &[child]);
    assert!(catalog.artifact("shop.OrderLine.entity", "1.0.0").is_some());
    let targets: Vec<String> = catalog
        .writes
        .iter()
        .map(|row| {
            assert_eq!(row.via, WriteVia::MutateRows);
            format!("{} {}", row.target.artifact_id, row.target.member.id())
        })
        .collect();
    assert_eq!(
        targets,
        [
            "shop.Order.entity type:shop.Order",
            "shop.OrderLine.entity type:shop.OrderLine"
        ]
    );
}

#[test]
fn installed_dependencies_of_installed_artifacts_are_projected() {
    // A candidate pack ships an installed action; the action's entity is
    // installed too and only the action references it.
    let pack = envelope(json!({
        "irVersion": "0.1", "artifactId": "commerce.invoicing.pack", "kind": "business_pack",
        "namespace": "commerce", "name": "Invoicing", "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "packId": "commerce.invoicing", "packVersion": "0.1.0", "packKind": "composition",
            "artifacts": [{
                "artifactId": "commerce.sales_order.submit_invoice.action",
                "kind": "action", "version": "0.1.0"
            }]
        }
    }));
    let catalog = catalog(&[pack], &[submit_invoice(), sales_order()]);
    let order = catalog
        .artifact("commerce.SalesOrder.entity", "0.1.0")
        .expect("the action's installed entity is pinned");
    assert!(!order.candidate);
    assert!(catalog.contains(&catalog.writes[0].target));
    assert_eq!(catalog.writes[0].target, invoice_transition());
}

fn request_types() -> [ArtifactEnvelope; 2] {
    let parent = entity(
        "shop.Request.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.Request",
            "properties": [{ "id": "status", "type": { "kind": "enum",
                "values": [{ "value": "open" }, { "value": "approved" }] }, "default": "open" }],
            "lifecycles": [{ "property": "status", "initial": ["open"], "terminal": ["approved"],
                "transitions": [{ "id": "approve", "from": ["open"], "to": "approved" }] }]
        }}),
    );
    let child = entity(
        "shop.RushRequest.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.RushRequest", "extends": "shop.Request", "properties": []
        }}),
    );
    [parent, child]
}

#[test]
fn a_law_on_an_inherited_transition_binds_the_declaring_supertype() {
    let [parent, child] = request_types();
    let Value::Array(laws) = read_json("fixtures/law-ir/laws.json") else {
        panic!("laws.json is a list");
    };
    let mut law = laws[0].clone();
    law["dependencies"] = json!([]);
    law["payload"]["subject"] = json!("shop.RushRequest");
    law["payload"]["response"] = json!({
        "kind": "transition", "objectType": "shop.RushRequest",
        "property": "status", "transitionId": "approve"
    });
    let catalog = catalog(&[parent, child, envelope(law)], &[]);
    let Some(LawBinding::Transition { target }) = &catalog.laws[0].binds else {
        panic!("the law binds a transition");
    };
    assert_eq!(target.artifact_id, "shop.Request.entity");
    assert!(catalog.contains(target));
}

fn request_member(member: Member) -> MemberRef {
    MemberRef {
        artifact_id: "shop.Request.entity".to_owned(),
        version: "1.0.0".to_owned(),
        member,
    }
}

#[test]
fn writes_and_mappings_of_inherited_members_bind_the_declaring_supertype() {
    let [parent, child] = request_types();
    let action = envelope(json!({
        "irVersion": "0.1", "artifactId": "shop.approve_rush.action", "kind": "action",
        "namespace": "shop", "name": "approve", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.approve_rush", "scopeRequired": [], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "effectProgram": {
                "kind": "fire_transition", "objectIdField": "id",
                "objectKind": "shop.RushRequest", "entityArtifactId": "shop.RushRequest.entity",
                "entityArtifactVersion": "1.0.0", "lifecycle": "status", "transition": "approve"
            }
        }
    }));
    let mapping = toy_mapping(&json!([{
        "id": "rush", "source": { "dataset": "requests" },
        "target": { "entity": "shop.RushRequest.entity", "interface": "shop.RushRequest.entity" },
        "subject": "input.id", "sourceRecord": "input.id",
        "lifecycles": [{ "property": "status", "state": "input.status" }],
        "gaps": [{ "member": "nowhere", "reason": "deferred" }]
    }]));
    let catalog = catalog(&[parent, child, action, mapping], &[]);
    assert_eq!(
        catalog.writes[0].target,
        request_member(Member::Transition {
            type_name: "shop.Request".to_owned(),
            lifecycle: "status".to_owned(),
            transition: "approve".to_owned(),
        })
    );
    let targets: Vec<&MemberRef> = catalog.mappings.iter().map(|row| &row.target).collect();
    // The inherited lifecycle property binds the parent's row; the object
    // row is the map's own type; an undeclared gap member has no row.
    assert_eq!(targets.len(), 2);
    assert!(targets.contains(&&request_member(Member::Property {
        type_name: "shop.Request".to_owned(),
        property: "status".to_owned(),
    })));
    assert!(targets.iter().all(|target| catalog.contains(target)));
}

#[test]
fn a_law_on_an_action_binds_its_pinned_version() {
    let action = |version: &str| {
        let mut action = read_json("fixtures/ir/commerce-sales-order-submit-invoice.action.json");
        action["version"] = json!(version);
        envelope(action)
    };
    let Value::Array(laws) = read_json("fixtures/law-ir/laws.json") else {
        panic!("laws.json is a list");
    };
    let mut law = laws[0].clone();
    law["payload"]["subject"] = json!("commerce.SalesOrder");
    law["payload"]["response"] =
        json!({ "kind": "action", "artifactId": "commerce.sales_order.submit_invoice.action" });
    law["dependencies"] = json!([{
        "artifactId": "commerce.sales_order.submit_invoice.action", "kind": "action",
        "version": "0.2.0", "required": true
    }]);
    let catalog = catalog(
        &[sales_order(), envelope(law)],
        &[action("0.1.0"), action("0.2.0")],
    );
    let Some(LawBinding::Action { target }) = &catalog.laws[0].binds else {
        panic!("the law binds an action");
    };
    assert_eq!(target.version, "0.2.0");
}

fn approve_action(id: &str, object_kind: &str, entity: &str, version: &str) -> ArtifactEnvelope {
    envelope(json!({
        "irVersion": "0.1", "artifactId": id, "kind": "action",
        "namespace": "shop", "name": "approve", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.approve", "scopeRequired": [], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "effectProgram": {
                "kind": "fire_transition", "objectIdField": "id",
                "objectKind": object_kind, "entityArtifactId": entity,
                "entityArtifactVersion": version, "lifecycle": "status", "transition": "approve"
            }
        }
    }))
}

#[test]
fn an_installed_supertype_reached_only_by_name_is_projected() {
    // The candidate subtype names its installed supertype through `extends`
    // and declares no envelope dependency on it.
    let [parent, child] = request_types();
    let action = approve_action(
        "shop.approve.action",
        "shop.RushRequest",
        "shop.RushRequest.entity",
        "1.0.0",
    );
    let catalog = catalog(&[child, action], &[parent]);
    let parent = catalog
        .artifact("shop.Request.entity", "1.0.0")
        .expect("the installed supertype is pinned");
    assert!(!parent.candidate);
    assert_eq!(catalog.writes.len(), 1);
    assert_eq!(catalog.writes[0].target.artifact_id, "shop.Request.entity");
    assert!(catalog.contains(&catalog.writes[0].target));
}

#[test]
fn a_transition_write_binds_the_version_its_action_pins() {
    let [parent, _] = request_types();
    let mut newer = serde_json::to_value(&parent).expect("envelope encodes");
    newer["version"] = json!("2.0.0");
    // Both versions are installed and reached; each action's write binds
    // the version it pins.
    let catalog = catalog(
        &[
            approve_action(
                "shop.approve.v1.action",
                "shop.Request",
                "shop.Request.entity",
                "1.0.0",
            ),
            approve_action(
                "shop.approve.v2.action",
                "shop.Request",
                "shop.Request.entity",
                "2.0.0",
            ),
        ],
        &[envelope(newer), parent],
    );
    let writes: Vec<(&str, &str)> = catalog
        .writes
        .iter()
        .map(|row| (row.writer.artifact_id.as_str(), row.target.version.as_str()))
        .collect();
    assert_eq!(
        writes,
        [
            ("shop.approve.v1.action", "1.0.0"),
            ("shop.approve.v2.action", "2.0.0")
        ]
    );
}

#[test]
fn a_supertype_name_binds_the_first_installed_artifact_like_the_entity_model() {
    // Two installed versions declare `shop.Request`; the candidate subtype
    // names it only through `extends`. The entity model binds the first one
    // in base order, and so does every catalog lookup.
    let [parent, child] = request_types();
    let mut newer = serde_json::to_value(&parent).expect("envelope encodes");
    newer["version"] = json!("2.0.0");
    let action = approve_action(
        "shop.approve.action",
        "shop.RushRequest",
        "shop.RushRequest.entity",
        "1.0.0",
    );
    let catalog = catalog(&[child, action], &[envelope(newer), parent]);
    assert!(catalog.artifact("shop.Request.entity", "2.0.0").is_some());
    assert!(catalog.artifact("shop.Request.entity", "1.0.0").is_none());
    assert_eq!(catalog.writes[0].target.version, "2.0.0");
}

#[test]
fn the_lineage_of_a_pinned_type_starts_at_the_pinned_version() {
    // Two installed versions of the subtype, the newer first in base order;
    // the action pins the older one, whose supertype declares the transition.
    let [parent, child] = request_types();
    let mut newer_child = serde_json::to_value(&child).expect("envelope encodes");
    newer_child["version"] = json!("2.0.0");
    let action = approve_action(
        "shop.approve.action",
        "shop.RushRequest",
        "shop.RushRequest.entity",
        "1.0.0",
    );
    let catalog = catalog(&[action], &[envelope(newer_child), child, parent]);
    assert_eq!(catalog.writes.len(), 1);
    assert_eq!(catalog.writes[0].target.artifact_id, "shop.Request.entity");
    assert!(catalog.contains(&catalog.writes[0].target));
}

#[test]
fn a_subtype_row_mutation_writes_its_inherited_child_types() {
    let parent = entity(
        "shop.Order.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.Order",
            "properties": [{ "id": "id", "type": { "kind": "text" } }],
            "links": [{ "id": "lines", "target": "shop.OrderLine",
                "cardinality": { "to": "many", "from": "one" } }],
            "ownedCollections": [{ "id": "lines", "childType": "shop.OrderLine",
                "rowIdProperty": "row_id", "orderingKey": "idx",
                "cascadeOnParentDelete": true, "maxRows": 20 }]
        }}),
    );
    let rush = entity(
        "shop.RushOrder.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.RushOrder", "extends": "shop.Order", "properties": []
        }}),
    );
    let child = entity(
        "shop.OrderLine.entity",
        &json!({ "entityVersion": "2.0", "objectType": {
            "name": "shop.OrderLine",
            "properties": [
                { "id": "row_id", "type": { "kind": "text" } },
                { "id": "idx", "type": { "kind": "integer" } }
            ]
        }}),
    );
    let action = envelope(json!({
        "irVersion": "0.1", "artifactId": "shop.edit_rush.action", "kind": "action",
        "namespace": "shop", "name": "edit", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.edit_rush", "scopeRequired": [], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
            "effectProgram": {
                "kind": "mutate_parent_with_rows", "objectIdField": "id",
                "objectKind": "shop.RushOrder", "entityArtifactId": "shop.RushOrder.entity",
                "entityArtifactVersion": "1.0.0", "parentValuesField": "values",
                "rowOpsField": "rows", "parentVersionField": "version",
                "collections": [{ "id": "lines" }]
            }
        }
    }));
    let catalog = catalog(&[parent, rush, child, action], &[]);
    let targets: Vec<String> = catalog
        .writes
        .iter()
        .map(|row| format!("{} {}", row.target.artifact_id, row.target.member.id()))
        .collect();
    assert_eq!(
        targets,
        [
            "shop.OrderLine.entity type:shop.OrderLine",
            "shop.RushOrder.entity type:shop.RushOrder"
        ]
    );
}

#[test]
fn a_row_mutation_writes_the_child_types_of_the_version_it_pins() {
    // Two installed versions of the parent declare different owned
    // collections; each action writes the child types of the version it pins.
    let parent = |version: &str, child: &str| {
        let mut parent = serde_json::to_value(entity(
            "shop.Order.entity",
            &json!({ "entityVersion": "2.0", "objectType": {
                "name": "shop.Order",
                "properties": [{ "id": "id", "type": { "kind": "text" } }],
                "links": [{ "id": "lines", "target": child,
                    "cardinality": { "to": "many", "from": "one" } }],
                "ownedCollections": [{ "id": "lines", "childType": child,
                    "rowIdProperty": "row_id", "orderingKey": "idx",
                    "cascadeOnParentDelete": true, "maxRows": 20 }]
            }}),
        ))
        .expect("envelope encodes");
        parent["version"] = json!(version);
        envelope(parent)
    };
    let child = |id: &str, name: &str| {
        entity(
            id,
            &json!({ "entityVersion": "2.0", "objectType": {
                "name": name,
                "properties": [
                    { "id": "row_id", "type": { "kind": "text" } },
                    { "id": "idx", "type": { "kind": "integer" } }
                ]
            }}),
        )
    };
    let action = |id: &str, version: &str| {
        envelope(json!({
            "irVersion": "0.1", "artifactId": id, "kind": "action",
            "namespace": "shop", "name": "edit", "version": "1.0.0", "lifecycle": "draft",
            "scope": {}, "dependencies": [], "provenance": { "source": "system" },
            "payload": {
                "actionId": "shop.edit_order", "scopeRequired": [], "idempotent": false,
                "ledgered": false, "inputSchema": { "type": "object" },
                "outputSchema": { "type": "object" },
                "effectProgram": {
                    "kind": "mutate_parent_with_rows", "objectIdField": "id",
                    "objectKind": "shop.Order", "entityArtifactId": "shop.Order.entity",
                    "entityArtifactVersion": version, "parentValuesField": "values",
                    "rowOpsField": "rows", "parentVersionField": "version",
                    "collections": [{ "id": "lines" }]
                }
            }
        }))
    };
    let catalog = catalog(
        &[
            action("shop.edit_v1.action", "1.0.0"),
            action("shop.edit_v2.action", "2.0.0"),
            child("shop.OrderLine.entity", "shop.OrderLine"),
            child("shop.OrderItem.entity", "shop.OrderItem"),
        ],
        &[
            parent("2.0.0", "shop.OrderItem"),
            parent("1.0.0", "shop.OrderLine"),
        ],
    );
    let targets: Vec<String> = catalog
        .writes
        .iter()
        .map(|row| {
            format!(
                "{}@{} {}",
                row.target.artifact_id,
                row.target.version,
                row.target.member.id()
            )
        })
        .collect();
    assert_eq!(
        targets,
        [
            "shop.edit_v1.action -> shop.Order.entity@1.0.0 type:shop.Order",
            "shop.edit_v1.action -> shop.OrderLine.entity@1.0.0 type:shop.OrderLine",
            "shop.edit_v2.action -> shop.Order.entity@2.0.0 type:shop.Order",
            "shop.edit_v2.action -> shop.OrderItem.entity@1.0.0 type:shop.OrderItem"
        ]
    );
}
