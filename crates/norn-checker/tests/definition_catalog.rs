#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Correspondence to verification/definition-catalog-v1/SPEC.md.
use norn_checker::definition_catalog::project;
use norn_checker::{EntityArtifactSetContext, check_artifact_set, resolve_artifact_set};
use norn_ir::definition_catalog::{
    ArtifactKey, DefinitionCatalog, DependencyVia, DependsOnRow, LawBinding, MapRule, Member,
    MemberRef, TypeKind, WriteVia,
};
use norn_ir::mapping::GapReason;
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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
    resolve_artifact_set(artifacts, base).expect("set resolves");
    project(artifacts, base, &BTreeSet::new()).expect("catalog projects")
}

fn admitted_catalog(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> DefinitionCatalog {
    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base,
            tenant: None,
        },
    );
    assert_eq!(findings, [], "{findings:?}");
    catalog(artifacts, base)
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

fn example_mapping() -> ArtifactEnvelope {
    envelope(json!({
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
    }))
}

#[test]
fn a_mapping_observes_members_and_declares_gaps() {
    let mapping = example_mapping();
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
    assert_eq!(catalog.writes, []);
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
    assert!(
        catalog
            .facets
            .iter()
            .all(|row| catalog.contains(&row.concept) && catalog.contains(&row.member))
    );
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
        "scope": {}, "dependencies": [{ "artifactId": "shop.OrderLine.entity", "kind": "entity", "required": true, "version": "1.0.0" }], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.edit_order", "scopeRequired": ["orgId"], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "values": { "type": ["null", "boolean", "number", "string", "array", "object"] }, "rows": { "type": "array", "items": { "type": ["null", "boolean", "number", "string", "array", "object"] } }, "version": { "type": "string" } }, "required": ["id", "values", "rows", "version"], "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": [{ "entityKind": "shop.Order", "change": "created" }, { "entityKind": "shop.Order", "change": "modified" }, { "entityKind": "shop.OrderLine", "change": "created" }, { "entityKind": "shop.OrderLine", "change": "modified" }, { "entityKind": "shop.OrderLine", "change": "deleted" }],
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
    let artifacts = [parent, action];
    let base = [child];
    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(findings, [], "{findings:?}");
    let catalog = catalog(&artifacts, &base);
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
    law["dependencies"] = json!([{ "artifactId": "shop.RushRequest.entity", "kind": "entity", "required": true, "version": "1.0.0" }]);
    law["payload"]["scope"]["objectTypes"] = json!(["shop.RushRequest"]);
    law["payload"]["subject"] = json!("shop.RushRequest");
    law["payload"]["response"] = json!({
        "kind": "transition", "objectType": "shop.RushRequest",
        "property": "status", "transitionId": "approve"
    });
    let artifacts = [parent, child, envelope(law)];
    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    );
    assert_eq!(findings, [], "{findings:?}");
    let catalog = catalog(&artifacts, &[]);
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
    let contract = entity(
        "shop.RequestScope.interface",
        &json!({ "entityVersion": "2.0", "interface": { "name": "shop.RequestScope", "properties": [] } }),
    );
    let mut child = serde_json::to_value(child).expect("child");
    child["payload"]["objectType"]["implements"] = json!(["shop.RequestScope"]);
    let child = envelope(child);
    let action = envelope(json!({
        "irVersion": "0.1", "artifactId": "shop.approve_rush.action", "kind": "action",
        "namespace": "shop", "name": "approve", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.approve_rush", "scopeRequired": ["orgId"], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": [{ "entityKind": "shop.RushRequest", "change": "modified" }],
            "effectProgram": {
                "kind": "fire_transition", "objectIdField": "id",
                "objectKind": "shop.RushRequest", "entityArtifactId": "shop.RushRequest.entity",
                "entityArtifactVersion": "1.0.0", "lifecycle": "status", "transition": "approve"
            }
        }
    }));
    let mapping = toy_mapping(&json!([{
        "id": "rush", "source": { "dataset": "requests" },
        "target": { "entity": "shop.RushRequest.entity", "interface": "shop.RequestScope.interface" },
        "subject": "input.id", "sourceRecord": "input.id",
        "lifecycles": [{ "property": "status", "state": "\"open\"" }]
    }]));
    let mut source = read_json("fixtures/ir/synthetic-contact.source_schema.json");
    source["artifactId"] = json!("shop.source");
    source["scope"] = json!({});
    source["payload"]["datasets"][0]["id"] = json!("requests");
    source["payload"]["datasets"][0]["nativeName"] = json!("requests");
    let field = source["payload"]["fields"][0].clone();
    source["payload"]["fields"] = json!([
        { "dataset": "requests", "name": "id", "nativeType": "text", "ordinal": 0, "required": true, "typeCandidate": field["typeCandidate"] },
        { "dataset": "requests", "name": "status", "nativeType": "text", "ordinal": 1, "required": true, "typeCandidate": field["typeCandidate"] }
    ]);
    let source = envelope(source);
    let mut mapping = serde_json::to_value(mapping).expect("mapping");
    mapping["payload"]["interfaces"] = json!(["shop.RequestScope.interface"]);
    mapping["dependencies"] = json!([
        { "artifactId": "shop.source", "kind": "source_schema", "required": true, "version": "1.0.0" },
        { "artifactId": "shop.RushRequest.entity", "kind": "entity", "required": true, "version": "1.0.0" },
        { "artifactId": "shop.RequestScope.interface", "kind": "entity", "required": true, "version": "1.0.0" }
    ]);
    mapping["payload"]["source"]["sha256"] = json!(
        norn_checker::receipt::ArtifactRef::of(&source)
            .expect("source hash")
            .sha256
    );
    let artifacts = [parent, child, action, envelope(mapping), source, contract];
    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    );
    assert_eq!(findings, [], "{findings:?}");
    let catalog = catalog(&artifacts, &[]);
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
    // row is the map's own type.
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
            "actionId": "shop.approve", "scopeRequired": ["orgId"], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": [{ "entityKind": object_kind, "change": "modified" }],
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
    let catalog = admitted_catalog(&[child, action], &[parent]);
    let parent = catalog
        .artifact("shop.Request.entity", "1.0.0")
        .expect("the installed supertype is pinned");
    assert!(!parent.candidate);
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
        "scope": {}, "dependencies": [{ "artifactId": "shop.OrderLine.entity", "kind": "entity", "required": true, "version": "1.0.0" }], "provenance": { "source": "system" },
        "payload": {
            "actionId": "shop.edit_rush", "scopeRequired": ["orgId"], "idempotent": false,
            "ledgered": false, "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "values": { "type": ["null", "boolean", "number", "string", "array", "object"] }, "rows": { "type": "array", "items": { "type": ["null", "boolean", "number", "string", "array", "object"] } }, "version": { "type": "string" } }, "required": ["id", "values", "rows", "version"], "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": [{ "entityKind": "shop.RushOrder", "change": "created" }, { "entityKind": "shop.RushOrder", "change": "modified" }, { "entityKind": "shop.OrderLine", "change": "created" }, { "entityKind": "shop.OrderLine", "change": "modified" }, { "entityKind": "shop.OrderLine", "change": "deleted" }],
            "effectProgram": {
                "kind": "mutate_parent_with_rows", "objectIdField": "id",
                "objectKind": "shop.RushOrder", "entityArtifactId": "shop.RushOrder.entity",
                "entityArtifactVersion": "1.0.0", "parentValuesField": "values",
                "rowOpsField": "rows", "parentVersionField": "version",
                "collections": [{ "id": "lines" }]
            }
        }
    }));
    let catalog = admitted_catalog(&[parent, rush, child, action], &[]);
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
fn two_installed_versions_of_one_type_bind_only_what_admission_binds() {
    // The checker does not admit two definitions of one type name: the
    // entity model reports a duplicate and neither version has an effective
    // view, so inherited members and owned collections fail closed. The
    // catalog binds exactly what admission binds: the pinned type's own
    // row, and nothing it would have to derive.
    let [parent, child] = request_types();
    let mut newer_child = serde_json::to_value(&child).expect("envelope encodes");
    newer_child["version"] = json!("2.0.0");
    let base = [envelope(newer_child), child, parent];
    let effective = norn_checker::EffectiveCatalog::from_artifacts(&base);
    assert!(matches!(
        effective.object_type("shop.RushRequest.entity"),
        Some(Err(_))
    ));
    let action = approve_action(
        "shop.approve.action",
        "shop.RushRequest",
        "shop.RushRequest.entity",
        "1.0.0",
    );
    let catalog = catalog(&[action], &base);
    // `approve` is inherited from `shop.Request`; with no admitted lineage
    // it binds nothing.
    assert_eq!(catalog.writes, []);
}

#[test]
fn a_decide_graph_writes_an_installed_type_it_names() {
    // The decision names `commerce.PurchaseOrder`, which only the installed
    // base defines, and declares no envelope dependency on it.
    let order = envelope(read_json(
        "fixtures/ir/entity-v2/artifact-set/commerce-purchase-order.entity.json",
    ));
    let rule = envelope(read_json(
        "fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json",
    ));
    let catalog = catalog(&[rule], &[order]);
    let mut writes: Vec<(String, String, WriteVia)> = catalog
        .writes
        .iter()
        .map(|row| {
            (
                row.target.artifact_id.clone(),
                row.target.member.id(),
                row.via,
            )
        })
        .collect();
    writes.sort();
    assert_eq!(
        writes,
        [
            (
                "commerce.PurchaseOrder.entity".to_owned(),
                "transition:commerce.PurchaseOrder#status/approve".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "commerce.PurchaseOrder.entity".to_owned(),
                "transition:commerce.PurchaseOrder#status/submit".to_owned(),
                WriteVia::FactTransition
            ),
            (
                "commerce.PurchaseOrder.entity".to_owned(),
                "type:commerce.PurchaseOrder".to_owned(),
                WriteVia::FactObject
            ),
        ]
    );
    assert!(
        catalog
            .writes
            .iter()
            .all(|row| catalog.contains(&row.target))
    );
}

fn case_artifact(entry: &Value) -> ArtifactEnvelope {
    let (fixture, overrides) = match entry {
        Value::String(path) => (path.as_str(), None),
        Value::Object(object) => (
            object["fixture"].as_str().expect("fixture path"),
            object.get("patch").and_then(Value::as_object),
        ),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_)) => {
            panic!("unexpected case artifact {other}")
        }
    };
    let mut value = read_json(&format!("fixtures/{fixture}"));
    for (key, member) in overrides.into_iter().flatten() {
        value[key] = member.clone();
    }
    envelope(value)
}

fn admitted_write_types(writer: &ArtifactEnvelope) -> BTreeSet<String> {
    let payload = serde_json::to_value(writer.payload.to_json_value()).expect("payload encodes");
    let strings = |value: &Value| -> Vec<String> {
        value.as_array().map_or_default(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
    };
    if payload.get("effectProgram").is_some() {
        payload["writes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|write| write["entityKind"].as_str().map(str::to_owned))
            .collect()
    } else {
        strings(&payload["outputSchema"]["properties"]["facts"]["items"]["properties"]["objectType"]["enum"])
            .into_iter()
            .collect()
    }
}

#[test]
fn over_every_admitted_fixture_set_the_catalog_writes_what_admission_admits() {
    let dir = format!("{ROOT}/fixtures/ir/artifact-set");
    let mut checked = 0;
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("case directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.to_string_lossy().ends_with(".case.json"))
        .collect();
    paths.sort();
    for path in paths {
        let case: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("case reads"))
                .expect("case parses");
        // Admitted registry-layer sets (a tenant layer needs its own context).
        if case["expect"]
            .as_array()
            .is_none_or(|expect| !expect.is_empty())
            || case.get("tenant").is_some()
        {
            continue;
        }
        let load = |key: &str| -> Vec<ArtifactEnvelope> {
            case[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(case_artifact)
                .collect()
        };
        let (artifacts, base) = (load("artifacts"), load("base"));
        let registry: BTreeSet<PropertyTypeName> = case["registry"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|name| PropertyTypeName(name.as_str().expect("registry name").to_owned()))
            .collect();
        let findings = check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &registry,
                base: &base,
                tenant: None,
            },
        );
        assert_eq!(
            findings,
            [],
            "{} is admitted: {:?}",
            path.display(),
            findings
                .iter()
                .map(|finding| finding.code.as_str())
                .collect::<Vec<_>>()
        );
        let catalog = project(&artifacts, &base, &registry).expect("catalog projects");
        for writer in artifacts.iter().chain(&base) {
            let written: BTreeSet<String> = catalog
                .writes
                .iter()
                .filter(|row| row.writer.artifact_id == writer.artifact_id)
                .filter_map(|row| match &row.target.member {
                    Member::Type { type_name }
                    | Member::Property { type_name, .. }
                    | Member::Transition { type_name, .. } => Some(type_name.clone()),
                    Member::Artifact {}
                    | Member::Link { .. }
                    | Member::State { .. }
                    | Member::ObjectMap { .. }
                    | Member::Law { .. } => None,
                })
                .collect();
            let is_writer = matches!(
                writer.kind(),
                norn_ir::ArtifactKind::Action | norn_ir::ArtifactKind::Rule
            );
            if !is_writer || (written.is_empty() && admitted_write_types(writer).is_empty()) {
                continue;
            }
            assert_eq!(
                written,
                admitted_write_types(writer),
                "{}: {}",
                path.display(),
                writer.artifact_id
            );
            assert!(
                catalog
                    .writes
                    .iter()
                    .all(|row| catalog.contains(&row.target))
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "the admitted fixture sets exercise writers ({checked})"
    );
}
#[test]
fn entity_projection_preserves_identity_declarations_and_order() {
    let artifact: ArtifactEnvelope = serde_json::from_value(serde_json::json!({
        "irVersion": "0.1", "artifactId": "commerce.SalesOrder.entity", "kind": "entity",
        "namespace": "commerce", "name": "SalesOrder", "version": "0.1.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": { "entityVersion": "2.0", "objectType": { "name": "commerce.SalesOrder",
            "properties": [{ "id": "order_number", "type": { "kind": "text" } }] } }
    }))
    .expect("fixture");
    let context = norn_checker::EntityArtifactSetContext {
        registry: &BTreeSet::new(),
        base: &[],
        tenant: None,
    };
    assert_eq!(
        norn_checker::check_artifact_set(std::slice::from_ref(&artifact), &context),
        []
    );
    let catalog =
        project(std::slice::from_ref(&artifact), &[], &BTreeSet::new()).expect("projection");
    let at = |member| MemberRef {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
        member,
    };
    assert!(catalog.contains(&at(Member::Type {
        type_name: "commerce.SalesOrder".into()
    })));
    assert_eq!(
        catalog
            .properties
            .iter()
            .map(|row| row.at.clone())
            .collect::<Vec<_>>(),
        [at(Member::Property {
            type_name: "commerce.SalesOrder".into(),
            property: "order_number".into()
        })]
    );
    assert_eq!(
        catalog,
        project(&[artifact], &[], &BTreeSet::new()).expect("repeat")
    );
}

#[test]
fn ambiguous_installed_mapping_targets_never_select_a_version() {
    let old = sales_order();
    let mut new = serde_json::to_value(&old).expect("envelope");
    new["version"] = json!("2.0.0");
    let projection = project(
        &[example_mapping()],
        &[old, envelope(new)],
        &BTreeSet::new(),
    )
    .expect("read-only projection");
    assert_eq!(projection.mappings, []);
    let old = sales_order();
    let mut new = serde_json::to_value(&old).expect("envelope");
    new["version"] = json!("2.0.0");
    let mut mapping = serde_json::to_value(example_mapping()).expect("mapping");
    mapping["dependencies"] = json!([{ "artifactId": "commerce.SalesOrder.entity", "kind": "entity", "version": "2.0.0", "required": true }]);
    let pinned = project(
        &[envelope(mapping)],
        &[old, envelope(new)],
        &BTreeSet::new(),
    )
    .expect("pinned projection");
    assert_ne!(pinned.mappings, []);
    assert!(
        pinned
            .mappings
            .iter()
            .all(|row| row.target.version == "2.0.0")
    );
}

#[test]
fn local_lifecycle_states_keep_inherited_property_ownership() {
    let mut parent: serde_json::Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/commerce.SalesOrder.entity.json"
    ))
    .expect("fixture");
    parent["payload"]["objectType"]["implements"] = serde_json::json!([]);
    parent["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .retain(|p| p["id"] == "status");
    parent["payload"]["objectType"]["identity"] = serde_json::json!([]);
    parent["payload"]["objectType"]["links"] = serde_json::json!([]);
    let mut child = parent.clone();
    child["artifactId"] = "commerce.Child.entity".into();
    child["payload"]["objectType"]["name"] = "commerce.Child".into();
    child["payload"]["objectType"]["extends"] = serde_json::json!("commerce.SalesOrder");
    child["payload"]["objectType"]["properties"] = serde_json::json!([]);
    parent["payload"]["objectType"]["lifecycles"] = serde_json::json!([]);
    let artifacts = [parent, child].map(|value| serde_json::from_value(value).expect("envelope"));
    let registry = BTreeSet::new();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = norn_checker::check_artifact_set(&artifacts, &context);
    assert_eq!(findings, [], "{findings:?}");
    let catalog = project(&artifacts, &[], &BTreeSet::new()).expect("projection");
    let state = MemberRef {
        artifact_id: "commerce.Child.entity".into(),
        version: "0.1.0".into(),
        member: Member::State {
            type_name: "commerce.Child".into(),
            lifecycle: "status".into(),
            state: "draft".into(),
        },
    };
    assert!(catalog.contains(&state));
    assert!(
        !catalog
            .properties
            .iter()
            .any(|row| row.at.artifact_id == state.artifact_id)
    );
    assert!(
        catalog
            .properties
            .iter()
            .any(|row| row.at.member.id() == "property:commerce.SalesOrder#status")
    );
}

#[test]
fn graph_catalog_retains_semantics_and_refuses_projection_writes() {
    let Value::Array(entities) = read_json("fixtures/law-ir/entities.json") else {
        panic!("entities")
    };
    let Value::Array(laws) = read_json("fixtures/law-ir/laws.json") else {
        panic!("laws")
    };
    let artifacts = [envelope(entities[0].clone()), envelope(laws[0].clone())];
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = check_artifact_set(&artifacts, &context);
    assert_eq!(findings, [], "{findings:?}");
    let retained = project(&[], &artifacts, &registry).expect("retained catalog");
    for artifact in &artifacts {
        let row = retained
            .artifact(&artifact.artifact_id, &artifact.version)
            .expect("exact pin");
        assert!(!row.candidate);
        assert_eq!(
            row.sha256,
            norn_checker::receipt::ArtifactRef::of(artifact)
                .expect("hash")
                .sha256
        );
    }
    let law = &retained.laws[0];
    assert_eq!(
        law.at.member,
        Member::Law {
            law: laws[0]["payload"]["id"].as_str().expect("law id").into()
        }
    );
    let transition = &retained.transitions[0];
    let source = &entities[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0];
    assert_eq!(
        serde_json::to_value(&transition.from).expect("states"),
        source["from"]
    );
    assert_eq!(transition.to, source["to"].as_str().expect("state"));
    assert_eq!(transition.guards, []);
    assert!(
        retained
            .facets
            .iter()
            .all(|row| retained.contains(&row.concept) && retained.contains(&row.member))
    );
    for payload in [
        serde_json::to_value(law).expect("law row"),
        serde_json::to_value(transition).expect("transition row"),
        serde_json::to_value(&retained).expect("catalog"),
    ] {
        let mut forged = entities[0].clone();
        forged["payload"] = payload;
        assert!(
            norn_checker::check_artifact_json(&forged)
                .iter()
                .any(|d| d.code.as_str() == "core.graph.catalog_write")
        );
    }
    assert_eq!(
        project(&[], &artifacts, &registry).expect("unchanged definitions"),
        retained
    );
}

proptest::proptest! {
    #[test]
    fn graph_catalog_edits_never_become_transition_definitions(target in "[a-z]{1,20}") {
        let [parent, child] = request_types();
        let retained = catalog(&[parent.clone(), child], &[]);
        let mut row = retained.transitions[0].clone();
        row.to = target;
        let mut forged = serde_json::to_value(&parent).expect("envelope");
        forged["payload"] = serde_json::to_value(row).expect("edited projection");
        proptest::prop_assert!(norn_checker::check_artifact_json(&forged).iter().any(|d| d.code.as_str() == "core.graph.catalog_write"));
    }
}

#[test]
fn graph_catalog_roles_follow_declarations_not_lifecycle_shape() {
    let [parent, _] = request_types();
    assert_eq!(
        catalog(std::slice::from_ref(&parent), &[]).types[0].role,
        norn_ir::ConceptRole::Thing
    );
    let mut old = serde_json::to_value(&parent).expect("parent");
    old["payload"]["objectType"]["role"] = json!("happening");
    let mut new = serde_json::to_value(&parent).expect("parent");
    new["version"] = json!("2.0.0");
    let versions = catalog(&[], &[envelope(old), envelope(new)]);
    assert_eq!(
        versions
            .types
            .iter()
            .map(|row| row.role)
            .collect::<Vec<_>>(),
        [norn_ir::ConceptRole::Happening, norn_ir::ConceptRole::Thing]
    );
    let marker = entity(
        "shop.Marker.interface",
        &json!({ "entityVersion": "2.0", "interface": { "name": "shop.Marker", "role": "happening" } }),
    );
    let mut specialized = serde_json::to_value(parent).expect("envelope");
    specialized["payload"]["objectType"]["implements"] = json!(["shop.Marker"]);
    let artifacts = [envelope(specialized), marker];
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    assert_eq!(check_artifact_set(&artifacts, &context), []);
    assert!(
        catalog(&artifacts, &[])
            .types
            .iter()
            .all(|row| row.role == norn_ir::ConceptRole::Happening)
    );
}

#[test]
fn defined_concept_wire_distinguishes_malformed_and_unsupported_semantics() {
    let declaration = envelope(json!({
        "irVersion": "0.1", "artifactId": "test.HighValue", "kind": "defined_concept",
        "namespace": "test", "name": "HighValue", "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [{ "artifactId": "test.Item.entity", "kind": "entity", "version": "1.0.0", "required": true }],
        "provenance": { "source": "system" }, "payload": {
            "id": "test.HighValue", "version": "1.0.0",
            "base": { "artifactId": "test.Item.entity", "version": "1.0.0", "member": { "kind": "type", "typeName": "test.Item" } },
            "predicate": "self.amount > 100", "dependencies": [],
            "bounds": norn_ir::ResourceLimitProfile::norn_limits_v1()
        }
    }));
    assert_eq!(norn_checker::check_artifact(&declaration), []);
    let wire = serde_json::to_value(&declaration).expect("wire");
    let positional = json!(["test.Item.entity", "1.0.0", {"kind":"type", "typeName":"test.Item"}]);
    let mut budget = wire["payload"]["bounds"].clone();
    budget["decide"] = json!([1, 1, 1]);
    for (field, value) in [
        ("base", positional.clone()),
        ("dependencies", json!([positional])),
        ("bounds", budget),
        ("base", json!(false)),
        ("bounds", json!({})),
        ("predicate", json!(1)),
        ("dependencies", json!([false])),
        ("members", json!([])),
    ] {
        let mut malformed = wire.clone();
        malformed["payload"][field] = value;
        let findings = norn_checker::check_artifact(&envelope(malformed));
        let codes: Vec<_> = findings.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(
            codes,
            ["defined_concept.validation.invalid_payload"],
            "{field}"
        );
    }
}

fn artifact(id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": "entity", "namespace": "test", "name": id,
        "version": "1.0.0", "lifecycle": "draft", "scope": {}, "dependencies": [],
        "provenance": { "source": "system" }, "payload": payload
    }))
    .expect("fixture")
}

fn check_ref_lifecycle(hops: usize) {
    let values = json!({ "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] });
    let types: Vec<_> = (0..hops).map(|i| json!({ "name": format!("test.state{i}"),
        "type": if i == 0 { values.clone() } else { json!({ "kind": "ref", "name": format!("test.state{}", i.saturating_sub(1)) }) } })).collect();
    let artifacts = [
        artifact(
            "test.types.entity",
            &json!({ "entityVersion": "2.0", "propertyTypes": types }),
        ),
        artifact(
            "test.Order.entity",
            &json!({ "entityVersion": "2.0", "objectType": { "name": "test.Order",
            "properties": [{ "id": "status", "type": { "kind": "ref", "name": format!("test.state{}", hops.saturating_sub(1)) } }],
            "lifecycles": [{ "property": "status", "initial": ["open"], "terminal": ["closed"],
                "transitions": [{ "id": "finish", "from": ["open"], "to": "closed" }] }] } }),
        ),
    ];
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = check_artifact_set(&artifacts, &context);
    assert_eq!(findings, [], "hops {hops}: {findings:?}");
    let catalog = project(&artifacts, &[], &registry).expect("projection");
    assert_eq!(
        json!(catalog.properties),
        json!([{
            "at": { "artifactId": "test.Order.entity", "version": "1.0.0",
                "member": { "kind": "property", "typeName": "test.Order", "property": "status" } },
            "computed": false,
            "lifecycle": { "states": ["open", "closed"], "initial": ["open"], "terminal": ["closed"] }
        }]),
        "hops {hops}"
    );
    assert_eq!(
        catalog,
        project(&artifacts, &[], &registry).expect("repeat"),
        "hops {hops}"
    );
}

#[test]
fn enum_alias_chains_preserve_lifecycle_metadata() {
    for hops in 1usize..8 {
        check_ref_lifecycle(hops);
    }
}
