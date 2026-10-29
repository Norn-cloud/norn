#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeSet;

use norn_checker::{EntityArtifactSetContext, TenantLayer, check_artifact_set};
use norn_ir::{ArtifactEnvelope, ExtensionNamespace};
use serde_json::{Value, json};

fn artifact(id: &str, kind: &str, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "fashion", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
        "provenance": {"source": "authoring_sdk"}, "payload": payload
    })
}

fn documents() -> Vec<Value> {
    vec![
        artifact(
            "fashion.types",
            "entity",
            json!({
                "entityVersion": "2.0", "propertyTypes": [
                    {"name": "fashion.code", "type": {"kind": "text"}},
                    {"name": "fashion.brand_code", "type": {"kind": "ref", "name": "fashion.code"}}
                ]
            }),
        ),
        artifact(
            "fashion.Base",
            "entity",
            json!({
                "entityVersion": "2.0", "objectType": {
                    "name": "fashion.Base", "properties": [
                        {"id": "code", "type": {"kind": "ref", "name": "fashion.code"}}
                    ]
                }
            }),
        ),
        artifact(
            "fashion.Brand",
            "entity",
            json!({
                "entityVersion": "2.0", "objectType": {
                    "name": "fashion.Brand", "extends": "fashion.Base", "properties": [
                        {"id": "active", "type": {"kind": "boolean"}}
                    ]
                }
            }),
        ),
        artifact(
            "fashion.Product",
            "entity",
            json!({
                "entityVersion": "2.0", "objectType": {
                    "name": "fashion.Product", "extends": "fashion.Base", "properties": [
                        {"id": "brand_code", "type": {"kind": "ref", "name": "fashion.brand_code"}}
                    ],
                    "links": [{"id": "brand", "target": "fashion.Brand",
                        "cardinality": {"to": "one", "from": "many", "required": true}}]
                }
            }),
        ),
        artifact(
            "fashion.submit",
            "action",
            json!({
                "actionId": "fashion.submit", "scopeRequired": ["orgId", "environmentId"],
                "idempotent": true, "ledgered": true,
            "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "inputSchema": {"type": "object", "properties": {
                    "code": {"type": ["string", "null"]}
                }, "additionalProperties": false},
                "derive": [
                    {"field": "code", "expression": "input.code ?? \"DEFAULT\"", "inputRequired": false},
                    {"field": "code", "expression": "upper(input.code)", "inputRequired": false}
                ],
                "slice": [
                    {"name": "product", "objectType": "fashion.Product",
                        "key": {"property": {"from": "input", "property": "code"}}},
                    {"name": "brand", "objectType": "fashion.Brand",
                        "key": {"property": {"from": "product", "property": "brand_code"}}},
                    {"name": "linked", "objectType": "fashion.Brand",
                        "key": {"link": {"from": "product", "link": "brand"}}}
                ],
                "preconditions": ["slice.brand.active", "slice.product.code == input.code", "len(input.code) > 0"]
            }),
        ),
    ]
}

fn findings(documents: Vec<Value>) -> Vec<norn_checker::Diagnostic> {
    let artifacts: Vec<ArtifactEnvelope> = documents
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact"))
        .collect();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

#[test]
fn related_records_inherited_catalog_types_and_derivation_refinement_are_admitted() {
    assert_eq!(findings(documents()), []);
}

fn cost_documents(expression: &str, with_slice: bool) -> Vec<Value> {
    let mut documents = documents();
    let action = &mut documents[4]["payload"];
    action["derive"] = json!([]);
    action["inputSchema"] = json!({
        "type": "object", "properties": {
            "code": {"type": "string"}, "body": {"type": "string"}
        }, "required": ["code", "body"], "additionalProperties": false
    });
    action["preconditions"] = json!([expression]);
    if !with_slice {
        action["slice"] = json!([]);
    }
    documents
}

#[test]
fn unrelated_slice_does_not_inflate_input_or_flat_alias_expression_cost() {
    for root in ["input.body", "body"] {
        let sum = vec![format!("len({root})"); 4].join(" + ");
        for with_slice in [false, true] {
            assert_eq!(
                findings(cost_documents(&format!("{sum} > 0"), with_slice)),
                [],
                "{root}, with_slice={with_slice}"
            );
        }
    }
}

#[test]
fn duplicated_input_aliases_and_costly_slices_still_exceed_decide_fuel() {
    let input = ["len(input.body)"; 16].join(" + ");
    let alias = ["len(body)"; 16].join(" + ");
    for with_slice in [false, true] {
        let found = findings(cost_documents(
            &format!("({input}) > 0 and ({alias}) > 0"),
            with_slice,
        ));
        assert!(
            found
                .iter()
                .any(|finding| finding.message.contains("expr.cost.exceeds_budget")),
            "aliases, with_slice={with_slice}: {found:?}"
        );
    }
    for expression in [
        ["len(slice.product.code)"; 4].join(" + "),
        format!(
            "{} + {}",
            ["len(slice.product.code)"; 3].join(" + "),
            ["len(input.body)"; 7].join(" + ")
        ),
    ] {
        let found = findings(cost_documents(&format!("{expression} > 0"), true));
        assert!(
            found
                .iter()
                .any(|finding| finding.message.contains("expr.cost.exceeds_budget")),
            "slice and input costs: {found:?}"
        );
    }
}

#[test]
fn independent_root_costs_still_enforce_aggregate_turn_fuel() {
    let expression = format!("{} > 0", ["len(input.body)"; 20].join(" + "));
    for with_slice in [false, true] {
        let mut documents = cost_documents(&expression, with_slice);
        documents[4]["payload"]["preconditions"] = json!(vec![expression.clone(); 8]);
        let found = findings(documents);
        assert!(
            found.iter().any(|finding| {
                finding.code == "action.validation.invalid_payload"
                    && finding.message.contains("aggregate static cost")
            }),
            "aggregate, with_slice={with_slice}: {found:?}"
        );
    }
}

#[test]
fn slice_preconditions_reject_unknown_fields_nonbooleans_and_implicit_links() {
    for expression in [
        "slice.brand.missing",
        "slice.product.code",
        "slice.product.brand.active",
        "slice.missing.active",
    ] {
        let mut documents = documents();
        documents[4]["payload"]["preconditions"] = json!([expression]);
        let findings = findings(documents);
        assert!(
            findings.iter().any(
                |finding| finding.code == "action.validation.invalid_payload"
                    && finding
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == "/payload/preconditions/0")
            ),
            "{expression}: {findings:?}"
        );
    }
}

#[test]
fn slice_property_and_link_selectors_are_resolved_against_the_model() {
    for (entry, key) in [
        (
            1,
            json!({"property": {"from": "product", "property": "missing"}}),
        ),
        (2, json!({"link": {"from": "product", "link": "missing"}})),
    ] {
        let mut documents = documents();
        documents[4]["payload"]["slice"][entry]["key"] = key;
        let findings = findings(documents);
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "artifact_set.action.invalid_slice"),
            "{findings:?}"
        );
    }
}

#[test]
fn unresolved_slice_catalogs_and_nullable_final_keys_fail_closed() {
    let mut unresolved = documents();
    unresolved[0]["payload"]["propertyTypes"][1]["type"]["name"] = json!("fashion.missing");
    assert!(!findings(unresolved).is_empty());

    let mut nullable = documents();
    nullable[4]["payload"]["derive"] = json!([
        {"field": "code", "expression": "input.code", "inputRequired": false}
    ]);
    let findings = findings(nullable);
    assert!(
        findings.iter().any(|finding| finding
            .span
            .as_ref()
            .is_some_and(|span| span.pointer == "/payload/slice/0/key/property")),
        "{findings:?}"
    );
}

#[test]
fn slice_link_selectors_admit_tenant_extension_links() {
    let mut documents = documents();
    let mut action = documents.pop().expect("action");
    action["scope"]["tenantId"] = json!("tenant_fashion");
    action["payload"]["slice"][2]["key"]["link"]["link"] = json!("fashion__brand");
    let mut extension = artifact(
        "fashion.Product.extension",
        "entity",
        json!({
            "entityVersion": "2.0", "objectTypeExtension": {
                "namespace": "fashion", "target": "fashion.Product",
                "links": [{"id": "fashion__brand", "target": "fashion.Brand",
                    "cardinality": {"to": "one", "from": "many", "required": true}}]
            }
        }),
    );
    extension["scope"]["tenantId"] = json!("tenant_fashion");
    let base: Vec<ArtifactEnvelope> = documents
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("base artifact"))
        .collect();
    let artifacts =
        [extension, action].map(|value| serde_json::from_value(value).expect("tenant artifact"));
    let namespace = ExtensionNamespace("fashion".to_owned());
    assert_eq!(
        check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: Some(TenantLayer {
                    tenant_id: "tenant_fashion",
                    namespace: &namespace,
                }),
            },
        ),
        [],
    );
}

#[test]
fn optional_slice_sources_require_optional_property_descendants_but_not_links() {
    let mut documents = documents();
    let action = &mut documents[4]["payload"];
    action["derive"] = json!([]);
    action["slice"][0]["optional"] = json!(true);
    action["slice"]
        .as_array_mut()
        .expect("slice entries")
        .push(json!({
            "name": "brand_again", "objectType": "fashion.Brand", "optional": true,
            "key": {"property": {"from": "brand", "property": "code"}}
        }));
    action["preconditions"] = json!([
        "slice.product == null or slice.brand != null",
        "slice.brand == null or slice.brand_again != null",
        "len(slice.linked) >= 0"
    ]);

    for (entry, pointer) in [
        (1, "/payload/slice/1/key/property"),
        (3, "/payload/slice/3/key/property"),
    ] {
        documents[4]["payload"]["slice"][1]["optional"] = json!(true);
        documents[4]["payload"]["slice"][3]["optional"] = json!(true);
        documents[4]["payload"]["slice"][entry]["optional"] = json!(false);
        let findings = findings(documents.clone());
        assert!(
            findings.iter().any(|finding| {
                finding.code == "artifact_set.action.invalid_slice"
                    && finding
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == pointer)
            }),
            "{pointer}: {findings:?}"
        );
    }

    documents[4]["payload"]["slice"][3]["optional"] = json!(true);
    assert_eq!(findings(documents), []);
}

#[test]
fn nullable_source_properties_require_optional_children_and_null_safe_conditions() {
    let mut documents = documents();
    documents[3]["payload"]["objectType"]["properties"][0]["constraints"] =
        json!({ "nullable": true });

    let required_child = findings(documents.clone());
    assert!(
        required_child.iter().any(|finding| {
            finding.code == "artifact_set.action.invalid_slice"
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/slice/1/key/property")
        }),
        "{required_child:?}"
    );

    documents[4]["payload"]["slice"][1]["optional"] = json!(true);
    documents[4]["payload"]["preconditions"] = json!([
        "slice.brand == null or slice.brand != null",
        "slice.product.code == input.code",
        "len(input.code) > 0"
    ]);
    assert_eq!(findings(documents), []);
}
