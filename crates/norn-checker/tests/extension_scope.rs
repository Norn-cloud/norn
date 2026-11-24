//! Security #1376 regression suite: extension, supertype, interface, and shared
//! property-type scope isolation across every admission consumer.

use std::collections::BTreeSet;

use norn_checker::artifact_set::codes::INCOMPATIBLE_SCOPE;
use norn_checker::{
    ArtifactRef, Diagnostic, EntityArtifactSetContext, check_artifact_set,
    check_artifact_set_entity_model,
};
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SCOPE_DIMENSIONS: [&str; 5] = [
    "orgId",
    "tenantId",
    "workspaceId",
    "environmentId",
    "businessUnitId",
];

fn decode(value: Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(value)?)
}

fn envelope(
    id: &str,
    kind: &str,
    scope: &Value,
    dependencies: &Value,
    payload: &Value,
) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    let namespace = id.split('.').next().unwrap_or("shop");
    decode(json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": kind,
        "namespace": namespace,
        "name": id,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": scope,
        "dependencies": dependencies,
        "provenance": { "source": "authoring_sdk" },
        "payload": payload,
    }))
}

fn base_sku(scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        "retail.Sku.entity",
        "entity",
        scope,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.Sku",
                "properties": [
                    { "id": "sku_code", "type": { "kind": "text" } },
                    { "id": "title", "type": { "kind": "text" } },
                    {
                        "id": "unit_price",
                        "type": {
                            "kind": "decimal",
                            "scale": 2,
                            "additivity": { "kind": "additive" }
                        }
                    },
                    {
                        "id": "status",
                        "type": {
                            "kind": "enum",
                            "values": [
                                { "value": "draft" },
                                { "value": "active" }
                            ]
                        },
                        "default": "draft"
                    }
                ],
                "identity": [{ "name": "primary", "properties": ["sku_code"] }],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["draft"],
                    "terminal": ["active"],
                    "transitions": [{
                        "id": "activate",
                        "from": ["draft"],
                        "to": "active"
                    }]
                }]
            }
        }),
    )
}

fn partner_entity(scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        "retail.Partner.entity",
        "entity",
        scope,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.Partner",
                "properties": [{ "id": "code", "type": { "kind": "text" } }],
                "identity": [{ "name": "primary", "properties": ["code"] }]
            }
        }),
    )
}

fn sku_extension(id: &str, scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        id,
        "entity",
        scope,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectTypeExtension": {
                "namespace": "acme",
                "target": "retail.Sku",
                "properties": [
                    {
                        "id": "acme__secret_tag",
                        "type": { "kind": "text" },
                        "constraints": { "nullable": true }
                    },
                    {
                        "id": "acme__secret_amount",
                        "type": {
                            "kind": "decimal",
                            "scale": 2,
                            "additivity": { "kind": "additive" }
                        },
                        "constraints": { "nullable": false },
                        "default": "0.00"
                    },
                    {
                        "id": "acme__secret_tier",
                        "type": {
                            "kind": "enum",
                            "values": [
                                { "value": "gold" },
                                { "value": "silver" }
                            ]
                        },
                        "default": "silver"
                    }
                ],
                "links": [{
                    "id": "acme__secret_partner",
                    "target": "retail.Partner",
                    "cardinality": { "to": "one", "from": "many" }
                }]
            }
        }),
    )
}

fn check_set(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &registry,
            base,
            tenant: None,
        },
    )
}

fn check_entities(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_artifact_set_entity_model(
        candidates,
        &EntityArtifactSetContext {
            registry: &registry,
            base,
            tenant: None,
        },
    )
}

fn errors_for<'a>(findings: &'a [Diagnostic], artifact_id: &str) -> Vec<&'a Diagnostic> {
    findings
        .iter()
        .filter(|d| d.is_error() && d.artifact_id.as_deref() == Some(artifact_id))
        .collect()
}

fn assert_refused(findings: &[Diagnostic], artifact_id: &str, context: &str) {
    let errors = errors_for(findings, artifact_id);
    assert!(
        !errors.is_empty(),
        "{context}: expected refusal for {artifact_id}, got: {findings:#?}"
    );
}

fn assert_admitted(findings: &[Diagnostic], artifact_id: &str, context: &str) {
    let errors = errors_for(findings, artifact_id);
    assert!(
        errors.is_empty(),
        "{context}: expected clean admission for {artifact_id}, got: {errors:#?}"
    );
}

fn has_scope_error(findings: &[Diagnostic], artifact_id: &str) -> bool {
    errors_for(findings, artifact_id)
        .iter()
        .any(|d| d.code.as_str() == INCOMPATIBLE_SCOPE)
}

fn scope_with(dim: &str, value: &str) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(dim.to_owned(), json!(value));
    Value::Object(map)
}

fn subtype(
    id: &str,
    name: &str,
    scope: &Value,
) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        id,
        "entity",
        scope,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": { "name": name, "extends": "retail.Sku", "properties": [] }
        }),
    )
}

fn reader_action(id: &str, scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        id,
        "action",
        scope,
        &json!([]),
        &json!({
            "actionId": id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sku_code": { "type": "string" },
                    "partner_id": { "type": "string" }
                },
                "required": ["sku_code", "partner_id"],
                "additionalProperties": false
            },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "retail.Sku",
                "objectIdField": "sku_code",
                "entityArtifactId": "retail.Sku.entity",
                "entityArtifactVersion": "1.0.0",
                "writeMode": { "kind": "create_only" }
            },
            "writes": [
                { "entityKind": "retail.Sku", "change": "created" }
            ],
            "slice": [
                {
                    "name": "sku",
                    "objectType": "retail.Sku",
                    "key": { "property": { "from": "input", "property": "sku_code" } }
                },
                {
                    "name": "partner",
                    "objectType": "retail.Partner",
                    "key": { "link": { "from": "sku", "link": "acme__secret_partner" } }
                },
                {
                    "name": "partner_target",
                    "objectType": "retail.Partner",
                    "key": { "property": { "from": "input", "property": "partner_id" } }
                }
            ],
            "aggregates": [{
                "name": "secret_total",
                "objectType": "retail.Sku",
                "aggregate": { "kind": "sum", "property": "acme__secret_amount" },
                "maxItems": 100
            }],
            "createLinks": [{
                "link": "acme__secret_partner",
                "targetType": "retail.Partner",
                "targetIdField": "partner_id"
            }],
            "preconditions": [
                "slice.sku.acme__secret_amount >= 0 and aggregates.secret_total >= 0"
            ]
        }),
    )
}

fn writer_action(id: &str, scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        id,
        "action",
        scope,
        &json!([]),
        &json!({
            "actionId": id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sku_code": { "type": "string" },
                    "acme__secret_tag": { "type": "string" }
                },
                "required": ["sku_code"],
                "additionalProperties": false
            },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectKind": "retail.Sku",
                "objectIdField": "sku_code",
                "entityArtifactId": "retail.Sku.entity",
                "entityArtifactVersion": "1.0.0"
            },
            "writes": [
                { "entityKind": "retail.Sku", "change": "created" },
                { "entityKind": "retail.Sku", "change": "modified" }
            ]
        }),
    )
}

fn decide_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["set"] },
                        "objectType": { "type": "string", "enum": ["retail.Sku"] },
                        "key": { "type": "string" },
                        "property": { "type": "string", "enum": ["acme__secret_tag"] },
                        "value": { "type": "string" }
                    },
                    "required": ["op", "objectType", "key", "property", "value"],
                    "additionalProperties": false
                }
            },
            "commands": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "actionId": { "type": "string" } },
                    "required": ["actionId"],
                    "additionalProperties": false
                }
            },
            "timers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "timerId": { "type": "string" },
                        "dueIn": { "type": "string" }
                    },
                    "required": ["timerId", "dueIn"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["facts", "commands", "timers"],
        "additionalProperties": false
    })
}

fn decide_rule(id: &str, scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        id,
        "rule",
        scope,
        &json!([]),
        &json!({
            "ruleId": id,
            "ruleVersion": "2.0",
            "contract": "decide",
            "scopeRequired": ["orgId", "environmentId"],
            "triggers": [{
                "on": "modified",
                "objectType": "retail.Sku",
                "changedProperties": ["acme__secret_tag"]
            }],
            "slice": [
                {
                    "name": "sku",
                    "objectType": "retail.Sku",
                    "key": "triggerObject"
                },
                {
                    "name": "partner",
                    "objectType": "retail.Partner",
                    "key": { "link": { "from": "sku", "link": "acme__secret_partner" } }
                }
            ],
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slice": {
                        "type": "object",
                        "properties": {
                            "sku": {
                                "type": "object",
                                "properties": { "sku_code": { "type": "string" } },
                                "required": ["sku_code"],
                                "additionalProperties": false
                            },
                            "partner": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": { "code": { "type": "string" } },
                                    "required": ["code"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["sku", "partner"],
                        "additionalProperties": false
                    },
                    "event": {
                        "type": "object",
                        "properties": { "kind": { "type": "string" } },
                        "required": ["kind"],
                        "additionalProperties": false
                    }
                },
                "required": ["slice", "event"],
                "additionalProperties": false
            },
            "outputSchema": decide_output_schema(),
            "graph": {
                "nodes": [
                    { "id": "in", "type": "inputNode", "name": "input" },
                    {
                        "id": "expr",
                        "type": "expressionNode",
                        "name": "emit",
                        "content": {
                            "expressions": [{
                                "id": "e1",
                                "key": "facts",
                                "value": "[{\"op\":\"set\",\"objectType\":\"retail.Sku\",\"key\":\"sku-1\",\"property\":\"acme__secret_tag\",\"value\":\"ok\"}]"
                            }]
                        }
                    },
                    { "id": "out", "type": "outputNode", "name": "output" }
                ],
                "edges": [
                    { "id": "e1", "sourceId": "in", "targetId": "expr" },
                    { "id": "e2", "sourceId": "expr", "targetId": "out" }
                ]
            }
        }),
    )
}

#[test]
fn entity_model_refuses_cross_scope_supertypes_across_all_dimensions() -> TestResult {
    let global = json!({});
    for dim in SCOPE_DIMENSIONS {
        let narrow_a = scope_with(dim, "alpha");
        let narrow_b = scope_with(dim, "beta");
        let super_a = base_sku(&narrow_a)?;
        let sub_global = subtype("retail.GlobalSub.entity", "retail.GlobalSub", &global)?;
        let sub_sibling = subtype("retail.BetaSub.entity", "retail.BetaSub", &narrow_b)?;
        let sub_same = subtype("retail.AlphaSub.entity", "retail.AlphaSub", &narrow_a)?;

        for in_base in [false, true] {
            let run = |sub: &ArtifactEnvelope| {
                if in_base {
                    check_entities(std::slice::from_ref(sub), std::slice::from_ref(&super_a))
                } else {
                    check_entities(&[super_a.clone(), sub.clone()], &[])
                }
            };
            assert!(has_scope_error(
                &run(&sub_global),
                "retail.GlobalSub.entity"
            ));
            assert!(has_scope_error(&run(&sub_sibling), "retail.BetaSub.entity"));
            assert!(!has_scope_error(&run(&sub_same), "retail.AlphaSub.entity"));
        }
    }
    Ok(())
}

#[test]
fn entity_model_refuses_cross_scope_interfaces_extensions_and_refs_across_all_dimensions()
-> TestResult {
    let global = json!({});
    for dim in SCOPE_DIMENSIONS {
        let narrow_a = scope_with(dim, "alpha");
        let super_a = base_sku(&narrow_a)?;
        let iface_a = envelope(
            "retail.Auditable.interface",
            "entity",
            &narrow_a,
            &json!([]),
            &json!({
                "entityVersion": "2.0",
                "interface": {
                    "name": "retail.Auditable",
                    "properties": [{ "id": "code", "type": { "kind": "text" } }]
                }
            }),
        )?;
        let impl_global = envelope(
            "retail.GlobalImpl.entity",
            "entity",
            &global,
            &json!([]),
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "retail.GlobalImpl",
                    "implements": ["retail.Auditable"],
                    "properties": [{ "id": "code", "type": { "kind": "text" } }]
                }
            }),
        )?;
        assert!(has_scope_error(
            &check_entities(&[impl_global], &[iface_a]),
            "retail.GlobalImpl.entity"
        ));

        let ext_global = sku_extension("acme.GlobalExt.entity", &global)?;
        assert!(has_scope_error(
            &check_entities(&[ext_global], &[super_a, partner_entity(&global)?]),
            "acme.GlobalExt.entity"
        ));

        let shared_a = envelope(
            "retail.Shared.types",
            "entity",
            &narrow_a,
            &json!([]),
            &json!({
                "entityVersion": "2.0",
                "propertyTypes": [
                    { "name": "retail.secret_code", "type": { "kind": "text" } }
                ]
            }),
        )?;
        let ref_global = envelope(
            "retail.GlobalRef.entity",
            "entity",
            &global,
            &json!([]),
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "retail.GlobalRef",
                    "properties": [
                        { "id": "code", "type": { "kind": "ref", "name": "retail.secret_code" } }
                    ]
                }
            }),
        )?;
        assert!(has_scope_error(
            &check_entities(&[ref_global], &[shared_a]),
            "retail.GlobalRef.entity"
        ));
    }
    Ok(())
}

#[test]
fn entity_invariant_cannot_read_out_of_scope_extension_property() -> TestResult {
    let global = json!({});
    let org_alpha = json!({ "orgId": "org_alpha" });
    let org_beta = json!({ "orgId": "org_beta" });
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let ext_alpha = sku_extension("acme.SkuExt.entity", &org_alpha)?;

    let subtype_with_inv = |id: &str, scope: &Value| {
        envelope(
            id,
            "entity",
            scope,
            &json!([]),
            &json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": id,
                    "extends": "retail.Sku",
                    "properties": [],
                    "invariants": [{
                        "id": "reads_ext",
                        "expression": "self.acme__secret_amount >= 0",
                        "message": "secret amount must be non-negative"
                    }]
                }
            }),
        )
    };

    for in_base in [false, true] {
        let base = if in_base {
            vec![sku.clone(), partner.clone(), ext_alpha.clone()]
        } else {
            vec![sku.clone(), partner.clone()]
        };
        let run = |sub: ArtifactEnvelope| {
            let cands = if in_base {
                vec![sub]
            } else {
                vec![ext_alpha.clone(), sub]
            };
            check_entities(&cands, &base)
        };
        assert_refused(
            &run(subtype_with_inv("retail.GlobalSub", &global)?),
            "retail.GlobalSub",
            &format!("in_base={in_base} global subtype invariant"),
        );
        assert_refused(
            &run(subtype_with_inv("retail.BetaSub", &org_beta)?),
            "retail.BetaSub",
            &format!("in_base={in_base} sibling subtype invariant"),
        );
        assert_admitted(
            &run(subtype_with_inv("retail.AlphaSub", &org_alpha)?),
            "retail.AlphaSub",
            &format!("in_base={in_base} same-scope subtype invariant"),
        );
    }
    Ok(())
}

#[test]
fn actions_cannot_read_aggregate_link_or_write_out_of_scope_extensions_across_all_dimensions()
-> TestResult {
    let global = json!({});
    for dim in SCOPE_DIMENSIONS {
        let narrow_a = scope_with(dim, "alpha");
        let narrow_b = scope_with(dim, "beta");
        let sku = base_sku(&global)?;
        let partner = partner_entity(&global)?;
        let ext_a = sku_extension("acme.SkuExt.entity", &narrow_a)?;

        for in_base in [false, true] {
            let base = if in_base {
                vec![sku.clone(), partner.clone(), ext_a.clone()]
            } else {
                vec![sku.clone(), partner.clone()]
            };
            let run = |action: ArtifactEnvelope| {
                let cands = if in_base {
                    vec![action]
                } else {
                    vec![ext_a.clone(), action]
                };
                check_set(&cands, &base)
            };

            assert_refused(
                &run(reader_action("retail.read_global", &global)?),
                "retail.read_global",
                &format!("dim={dim} in_base={in_base} global reader action"),
            );
            assert_refused(
                &run(reader_action("retail.read_beta", &narrow_b)?),
                "retail.read_beta",
                &format!("dim={dim} in_base={in_base} sibling reader action"),
            );
            assert_admitted(
                &run(reader_action("retail.read_alpha", &narrow_a)?),
                "retail.read_alpha",
                &format!("dim={dim} in_base={in_base} same-scope reader action"),
            );

            assert!(has_scope_error(
                &run(writer_action("retail.write_global", &global)?),
                "retail.write_global"
            ));
            assert!(has_scope_error(
                &run(writer_action("retail.write_beta", &narrow_b)?),
                "retail.write_beta"
            ));
            assert_admitted(
                &run(writer_action("retail.write_alpha", &narrow_a)?),
                "retail.write_alpha",
                &format!("dim={dim} in_base={in_base} same-scope writer action"),
            );
        }
    }
    Ok(())
}

#[test]
fn lifecycle_writes_and_rule_bindings_cannot_target_out_of_scope_extensions() -> TestResult {
    let global = json!({});
    let tenant_a = json!({ "tenantId": "tenant_a" });
    let tenant_b = json!({ "tenantId": "tenant_b" });
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let ext_a = sku_extension("acme.SkuExt.entity", &tenant_a)?;

    for in_base in [false, true] {
        let base = if in_base {
            vec![sku.clone(), partner.clone(), ext_a.clone()]
        } else {
            vec![sku.clone(), partner.clone()]
        };
        let run = |rule: ArtifactEnvelope| {
            let cands = if in_base {
                vec![rule]
            } else {
                vec![ext_a.clone(), rule]
            };
            check_set(&cands, &base)
        };

        assert_refused(
            &run(decide_rule("retail.rule_global", &global)?),
            "retail.rule_global",
            &format!("in_base={in_base} global decide rule"),
        );
        assert_refused(
            &run(decide_rule("retail.rule_b", &tenant_b)?),
            "retail.rule_b",
            &format!("in_base={in_base} sibling decide rule"),
        );
        assert_admitted(
            &run(decide_rule("retail.rule_a", &tenant_a)?),
            "retail.rule_a",
            &format!("in_base={in_base} same-scope decide rule"),
        );
    }
    Ok(())
}

fn assert_consumer_scopes(
    build: impl Fn(&str, &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>>,
    prefix: &str,
    extra_base: &[ArtifactEnvelope],
) -> TestResult {
    let global = json!({});
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let same = json!({ "orgId": "org_a", "businessUnitId": "bu_a" });
    let ext_a = sku_extension("acme.SkuExt.entity", &same)?;
    let broader = json!({ "orgId": "org_a" });
    let sibling = json!({ "orgId": "org_a", "businessUnitId": "bu_b" });
    let narrower = json!({
        "orgId": "org_a",
        "businessUnitId": "bu_a",
        "workspaceId": "ws_child"
    });

    for in_base in [false, true] {
        let mut base = vec![sku.clone(), partner.clone()];
        base.extend(extra_base.iter().cloned());
        if in_base {
            base.push(ext_a.clone());
        }
        let run =
            |suffix: &str, scope: &Value| -> Result<Vec<Diagnostic>, Box<dyn std::error::Error>> {
                let id = format!("retail.{prefix}_{suffix}");
                let consumer = build(&id, scope)?;
                let cands = if in_base {
                    vec![consumer]
                } else {
                    vec![ext_a.clone(), consumer]
                };
                Ok(check_set(&cands, &base))
            };

        let id_broader = format!("retail.{prefix}_broader");
        let id_sibling = format!("retail.{prefix}_sibling");
        let id_same = format!("retail.{prefix}_same");
        let id_narrow = format!("retail.{prefix}_narrow");

        let f_broader = run("broader", &broader)?;
        assert!(
            !errors_for(&f_broader, &id_broader).is_empty(),
            "{prefix} in_base={in_base}: broader must fail: {f_broader:#?}"
        );
        let f_sibling = run("sibling", &sibling)?;
        assert!(
            !errors_for(&f_sibling, &id_sibling).is_empty(),
            "{prefix} in_base={in_base}: sibling must fail: {f_sibling:#?}"
        );
        let f_same = run("same", &same)?;
        assert!(
            errors_for(&f_same, &id_same).is_empty(),
            "{prefix} in_base={in_base}: same scope must succeed: {f_same:#?}"
        );
        let f_narrow = run("narrow", &narrower)?;
        assert!(
            errors_for(&f_narrow, &id_narrow).is_empty(),
            "{prefix} in_base={in_base}: narrower scope must succeed: {f_narrow:#?}"
        );
    }
    Ok(())
}

#[test]
fn laws_cannot_observe_out_of_scope_extensions() -> TestResult {
    let sku_dep = json!([{
        "artifactId": "retail.Sku.entity",
        "kind": "entity",
        "version": "1.0.0",
        "required": true
    }]);
    assert_consumer_scopes(
        |id, scope| {
            envelope(
                id,
                "law",
                scope,
                &sku_dep,
                &json!({
                    "lawVersion": "0.1",
                    "id": id,
                    "owner": "retail",
                    "provenance": { "source": "authoring_sdk" },
                    "scope": { "root": { "kind": "company", "id": "org_a" }, "objectTypes": ["retail.Sku"] },
                    "subject": "retail.Sku",
                    "modality": { "kind": "always" },
                    "response": { "kind": "predicate", "expression": "self.acme__secret_amount >= 0" },
                    "severity": "error",
                    "rationale": "secret amount non-negative"
                }),
            )
        },
        "law",
        &[],
    )
}

#[test]
fn models_cannot_observe_out_of_scope_extensions() -> TestResult {
    assert_consumer_scopes(
        |id, scope| {
            envelope(
                id,
                "model",
                scope,
                &json!([]),
                &json!({
                    "modelId": id,
                    "title": "Secret model",
                    "version": "1.0.0",
                    "input": { "kind": "objectType", "objectType": "retail.Sku" },
                    "features": ["acme__secret_amount"],
                    "outputs": [
                        { "id": "score", "type": { "kind": "decimal", "scale": 2 }, "nullable": false }
                    ],
                    "trust": "opaque",
                    "method": { "label": "linear" }
                }),
            )
        },
        "model",
        &[],
    )
}

#[test]
fn views_cannot_observe_out_of_scope_extensions() -> TestResult {
    let sku_dep = json!([{
        "artifactId": "retail.Sku.entity",
        "kind": "entity",
        "version": "1.0.0",
        "required": true
    }]);
    assert_consumer_scopes(
        |id, scope| {
            envelope(
                id,
                "derived",
                scope,
                &sku_dep,
                &json!({
                    "viewVersion": "0.1",
                    "viewId": id,
                    "inputs": [{
                        "alias": "skus",
                        "kind": "entity",
                        "artifactId": "retail.Sku.entity",
                        "version": "1.0.0"
                    }],
                    "outputSchema": {},
                    "maintenance": "recompute_on_read",
                    "calc": {
                        "kind": "relational",
                        "output": "proj",
                        "nodes": {
                            "scan": { "op": "scan", "input": "skus" },
                            "proj": {
                                "op": "project",
                                "input": "scan",
                                "columns": [
                                    { "name": "id", "expression": "input.id" },
                                    { "name": "sku_code", "expression": "input.sku_code" },
                                    { "name": "secret", "expression": "input.acme__secret_amount" }
                                ]
                            }
                        }
                    }
                }),
            )
        },
        "view",
        &[],
    )
}

#[test]
fn labels_cannot_target_out_of_scope_extensions() -> TestResult {
    assert_consumer_scopes(
        |id, scope| {
            envelope(
                id,
                "labels",
                scope,
                &json!([]),
                &json!({
                    "labelsVersion": "0.1",
                    "entries": [{
                        "target": {
                            "kind": "property",
                            "objectType": "retail.Sku",
                            "property": "acme__secret_tag"
                        },
                        "label": { "en": "Secret tag" }
                    }]
                }),
            )
        },
        "labels",
        &[],
    )
}

fn mapping_source_schema() -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    envelope(
        "erp.source_schema",
        "source_schema",
        &json!({}),
        &json!([]),
        &json!({
            "format": "norn.source-schema/1",
            "source": { "system": "ERPNext", "instance": "erp.example.test", "version": "15.0.0" },
            "observedAt": "2026-09-30T10:00:00Z",
            "adapter": { "name": "norn.importers.frappe", "version": "0.1.0" },
            "rawMetadata": {
                "uri": "evidence://frappe/erp.example.test/meta/items.json",
                "sha256": "91442427d8192f8908dc1d972b02508709de5bd3b700cb4faa76eeda0a3d5c7b"
            },
            "datasets": [{ "id": "Item", "nativeName": "Item", "namespace": "Stock", "kind": "document" }],
            "fields": [
                {
                    "dataset": "Item",
                    "name": "name",
                    "ordinal": 0,
                    "nativeType": "Data",
                    "typeCandidate": { "type": "string", "origin": "declared" },
                    "required": true
                },
                {
                    "dataset": "Item",
                    "name": "item_name",
                    "ordinal": 1,
                    "nativeType": "Data",
                    "typeCandidate": { "type": "string", "origin": "declared" },
                    "required": true
                },
                {
                    "dataset": "Item",
                    "name": "secret_tag",
                    "ordinal": 2,
                    "nativeType": "Data",
                    "typeCandidate": { "type": "string", "origin": "declared" },
                    "required": false
                }
            ]
        }),
    )
}

fn mapping_entities(
    ext_scope: &Value,
) -> Result<(ArtifactEnvelope, ArtifactEnvelope, ArtifactEnvelope), Box<dyn std::error::Error>> {
    let iface = envelope(
        "retail.CatalogItem.interface",
        "entity",
        &json!({}),
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "interface": {
                "name": "retail.CatalogItem",
                "properties": [{ "id": "sku_code", "type": { "kind": "text" } }]
            }
        }),
    )?;
    let sku_impl = envelope(
        "retail.MappedSku.entity",
        "entity",
        &json!({}),
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.MappedSku",
                "implements": ["retail.CatalogItem"],
                "properties": [
                    { "id": "sku_code", "type": { "kind": "text" } },
                    { "id": "title", "type": { "kind": "text" } }
                ],
                "identity": [{ "name": "primary", "properties": ["sku_code"] }]
            }
        }),
    )?;
    let mapped_ext = envelope(
        "acme.MappedSkuExt.entity",
        "entity",
        ext_scope,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectTypeExtension": {
                "namespace": "acme",
                "target": "retail.MappedSku",
                "properties": [{
                    "id": "acme__secret_tag",
                    "type": { "kind": "text" },
                    "constraints": { "nullable": true }
                }]
            }
        }),
    )?;
    Ok((iface, sku_impl, mapped_ext))
}

#[test]
fn mappings_cannot_write_out_of_scope_extensions() -> TestResult {
    let ext_same = json!({ "orgId": "org_a", "businessUnitId": "bu_a" });
    let (iface, sku_impl, mapped_ext) = mapping_entities(&ext_same)?;
    let source_schema = mapping_source_schema()?;
    let source_hash = ArtifactRef::of(&source_schema)?.sha256;
    let build = |id: &str, scope: &Value| {
        envelope(
            id,
            "mapping",
            scope,
            &json!([
                { "artifactId": "erp.source_schema", "kind": "source_schema", "version": "1.0.0", "required": true },
                { "artifactId": "retail.CatalogItem.interface", "kind": "entity", "version": "1.0.0", "required": true },
                { "artifactId": "retail.MappedSku.entity", "kind": "entity", "version": "1.0.0", "required": true }
            ]),
            &json!({
                "format": "norn.mapping/1",
                "source": {
                    "schemaId": "erp.source_schema",
                    "version": "1.0.0",
                    "sha256": source_hash
                },
                "interfaces": ["retail.CatalogItem.interface"],
                "maps": [{
                    "id": "sku",
                    "source": { "dataset": "Item" },
                    "target": {
                        "entity": "retail.MappedSku.entity",
                        "interface": "retail.CatalogItem.interface"
                    },
                    "subject": "input.name",
                    "sourceRecord": "input.name",
                    "properties": [
                        { "property": "sku_code", "value": "input.name" },
                        { "property": "title", "value": "input.item_name" },
                        { "property": "acme__secret_tag", "value": "input.secret_tag" }
                    ]
                }]
            }),
        )
    };

    let broader = json!({ "orgId": "org_a" });
    let sibling = json!({ "orgId": "org_a", "businessUnitId": "bu_b" });
    let narrower = json!({
        "orgId": "org_a",
        "businessUnitId": "bu_a",
        "workspaceId": "ws_child"
    });
    for in_base in [false, true] {
        let mut base = vec![iface.clone(), sku_impl.clone(), source_schema.clone()];
        if in_base {
            base.push(mapped_ext.clone());
        }
        let run =
            |id: &str, scope: &Value| -> Result<Vec<Diagnostic>, Box<dyn std::error::Error>> {
                let mapping = build(id, scope)?;
                let cands = if in_base {
                    vec![mapping]
                } else {
                    vec![mapped_ext.clone(), mapping]
                };
                Ok(check_set(&cands, &base))
            };
        assert_refused(
            &run("retail.map_broader", &broader)?,
            "retail.map_broader",
            "broader mapping",
        );
        assert_refused(
            &run("retail.map_sibling", &sibling)?,
            "retail.map_sibling",
            "sibling mapping",
        );
        assert_admitted(
            &run("retail.map_same", &ext_same)?,
            "retail.map_same",
            "same-scope mapping",
        );
        assert_admitted(
            &run("retail.map_narrow", &narrower)?,
            "retail.map_narrow",
            "narrower mapping",
        );
    }
    Ok(())
}
