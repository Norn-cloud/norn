//! Security #1376 regression suite (part 2): laws, models, views, labels,
//! mappings, defined concepts, derived metrics, definition catalog, flow
//! source maps, and scope-bound admission receipts.

use std::collections::BTreeSet;

use norn_checker::entity_scopes::derive_receipt_type_env_for_scope;
use norn_checker::metric::derive::{derive_metric_space, derive_metric_space_for_scope};
use norn_checker::{
    ArtifactRef, Diagnostic, EffectiveCatalog, EntityArtifactSetContext, ReceiptSources,
    check_artifact_set, check_artifact_set_with_receipt, definition_catalog, derive_type_env,
    validate_flow_source_map, verify_admission_receipt,
};
use norn_ir::{
    ArtifactEnvelope, FlowSourceMap, LawRoot, PropertyTypeName, ResourceLimitProfile, RuntimeValue,
    ScopeRef,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

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
                    { "id": "recorded_at", "type": { "kind": "timestamp", "timezone": "utc" } },
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

#[test]
fn whole_set_catalog_metrics_definition_catalog_and_flow_map_do_not_leak_narrower_extensions()
-> TestResult {
    let global = json!({});
    let tenant_a = json!({ "tenantId": "tenant_a" });
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let ext_a = sku_extension("acme.SkuExt.entity", &tenant_a)?;
    let pool = vec![sku.clone(), partner, ext_a];
    let reg = BTreeSet::new();

    // EffectiveCatalog::from_artifacts must not merge narrower extension into global retail.Sku.
    let unscoped = EffectiveCatalog::from_artifacts(&pool);
    let global_sku = unscoped
        .object_type_named("retail.Sku")
        .ok_or("missing retail.Sku")?
        .map_err(|d| format!("unexpected blocking diagnostic: {d:?}"))?;
    assert!(global_sku.property("acme__secret_amount").is_none());
    assert!(
        unscoped
            .links_for_object_type("retail.Sku")
            .unwrap_or_default()
            .iter()
            .all(|link| link.id.as_str() != "acme__secret_partner")
    );

    // EffectiveCatalog::for_scope(tenant_a) sees the extension; for_scope(tenant_b) does not.
    let alpha_scope = ScopeRef {
        tenant_id: Some("tenant_a".to_owned()),
        ..ScopeRef::default()
    };
    let beta_scope = ScopeRef {
        tenant_id: Some("tenant_b".to_owned()),
        ..ScopeRef::default()
    };
    let alpha_cat = EffectiveCatalog::for_scope(&pool, &reg, &alpha_scope);
    let alpha_sku = alpha_cat
        .object_type_named("retail.Sku")
        .ok_or("missing alpha retail.Sku")?
        .map_err(|d| format!("alpha blocking: {d:?}"))?;
    assert!(alpha_sku.property("acme__secret_amount").is_some());
    let beta_cat = EffectiveCatalog::for_scope(&pool, &reg, &beta_scope);
    let beta_sku = beta_cat
        .object_type_named("retail.Sku")
        .ok_or("missing beta retail.Sku")?
        .map_err(|d| format!("beta blocking: {d:?}"))?;
    assert!(beta_sku.property("acme__secret_amount").is_none());

    // Metric derivation: unscoped derive_metric_space must not emit candidates for narrower extension.
    let law_root = LawRoot::Cell("global".to_owned());
    let unscoped_space = derive_metric_space(&pool, &reg, &law_root)
        .map_err(|d| format!("unscoped metric derive failed: {d:?}"))?;
    assert!(
        unscoped_space
            .candidates()
            .iter()
            .all(|c| !c.definition.name.contains("acme__secret_amount"))
    );
    let alpha_space = derive_metric_space_for_scope(&pool, &reg, &law_root, &alpha_scope)
        .map_err(|d| format!("alpha metric derive failed: {d:?}"))?;
    assert!(
        alpha_space
            .candidates()
            .iter()
            .any(|c| c.definition.name.contains("acme__secret_amount"))
    );

    // Definition catalog keeps extension properties owned by the extension artifact.
    let def_catalog = definition_catalog::project(&pool, &[], &reg)?;
    assert!(!def_catalog.properties.iter().any(|row| {
        row.at.artifact_id == "retail.Sku.entity"
            && matches!(
                &row.at.member,
                norn_ir::definition_catalog::Member::Property { property, .. }
                    if property == "acme__secret_amount"
            )
    }));

    // Flow source map validates states strictly against each entity's own scope catalog.
    let sku_hash = ArtifactRef::of(&sku)?.sha256;
    let flow_map: FlowSourceMap = serde_json::from_value(json!({
        "format": "norn-flow-source-map/1",
        "flows": [{
            "flowId": "sku-flow",
            "flowVersion": "1.0.0",
            "sourcePath": "flows/sku.ts",
            "artifactRefs": [{ "artifactId": "retail.Sku.entity", "version": "1.0.0", "sha256": sku_hash }],
            "steps": [{ "id": "s1", "kind": "perform", "artifactIds": ["retail.Sku.entity"], "stateIds": ["draft"], "decisionRowIds": [] }],
            "edges": []
        }]
    }))?;
    assert!(
        validate_flow_source_map(&flow_map, &pool, &[]).is_empty(),
        "valid global state in flow source map should pass"
    );
    Ok(())
}

#[test]
fn defined_concepts_refuse_out_of_scope_transitive_dependencies_and_extensions() -> TestResult {
    let global = json!({});
    let tenant_a = json!({ "tenantId": "tenant_a" });
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let ext_a = sku_extension("acme.SkuExt.entity", &tenant_a)?;
    let bounds = serde_json::to_value(ResourceLimitProfile::norn_limits_v1())?;

    let dc_reading_ext = envelope(
        "retail.SecretConcept.dc",
        "defined_concept",
        &global,
        &json!([{
            "artifactId": "retail.Sku.entity",
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
        &json!({
            "id": "retail.SecretConcept",
            "version": "1.0.0",
            "base": {
                "artifactId": "retail.Sku.entity",
                "version": "1.0.0",
                "member": { "kind": "type", "typeName": "retail.Sku" }
            },
            "predicate": "self.acme__secret_amount >= 0",
            "dependencies": [],
            "bounds": bounds
        }),
    )?;
    let findings = check_set(&[dc_reading_ext], &[sku, partner, ext_a]);
    assert_refused(
        &findings,
        "retail.SecretConcept.dc",
        "global DefinedConcept must not read narrower extension property",
    );

    // Transitive out-of-scope base dependency: middle (global) depends on leaf (tenant_a);
    // global DefinedConcept targeting middle must fail closed on transitive closure scope check.
    let leaf_a = envelope(
        "retail.LeafAlpha.entity",
        "entity",
        &tenant_a,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.LeafAlpha",
                "properties": [{ "id": "code", "type": { "kind": "text" } }],
                "identity": [{ "name": "primary", "properties": ["code"] }]
            }
        }),
    )?;
    let mid_global = envelope(
        "retail.MidGlobal.entity",
        "entity",
        &global,
        &json!([{
            "artifactId": "retail.LeafAlpha.entity",
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.MidGlobal",
                "properties": [{ "id": "code", "type": { "kind": "text" } }],
                "identity": [{ "name": "primary", "properties": ["code"] }]
            }
        }),
    )?;
    let dc_mid = envelope(
        "retail.MidConcept.dc",
        "defined_concept",
        &global,
        &json!([{
            "artifactId": "retail.MidGlobal.entity",
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
        &json!({
            "id": "retail.MidConcept",
            "version": "1.0.0",
            "base": {
                "artifactId": "retail.MidGlobal.entity",
                "version": "1.0.0",
                "member": { "kind": "type", "typeName": "retail.MidGlobal" }
            },
            "predicate": "self.code == 'x'",
            "dependencies": [],
            "bounds": bounds
        }),
    )?;
    let dc_findings = check_set(&[dc_mid], &[leaf_a, mid_global]);
    assert_refused(
        &dc_findings,
        "retail.MidConcept.dc",
        "DefinedConcept must refuse out-of-scope transitive dependency",
    );
    Ok(())
}

#[test]
fn receipt_type_env_is_scope_bound_and_never_leaks_narrower_extensions() -> TestResult {
    let global = json!({});
    let ws_a = json!({ "workspaceId": "ws_alpha" });
    let ws_b = json!({ "workspaceId": "ws_beta" });
    let sku = base_sku(&global)?;
    let partner = partner_entity(&global)?;
    let ext_a = sku_extension("acme.SkuExt.entity", &ws_a)?;
    let ext_b = sku_extension("acme.SkuExt.entity", &ws_b)?;
    let limits = ResourceLimitProfile::norn_limits_v1();

    // Unscoped derive_type_env must NOT merge narrower extension into global retail.Sku.
    let base = vec![sku, partner];
    let unscoped_env = derive_type_env(std::slice::from_ref(&ext_a), &base, &limits)
        .map_err(|e| format!("derive_type_env failed: {e:?}"))?;
    assert!(
        unscoped_env
            .record("retail.Sku")
            .and_then(|rec| rec.fields.get("acme__secret_amount"))
            .is_none(),
        "unscoped derive_type_env leaked narrower extension field onto global type"
    );

    // Consumer-scoped derive_receipt_type_env_for_scope exposes extension ONLY to visible scopes.
    let alpha_scope = ScopeRef {
        workspace_id: Some("ws_alpha".to_owned()),
        ..ScopeRef::default()
    };
    let beta_scope = ScopeRef {
        workspace_id: Some("ws_beta".to_owned()),
        ..ScopeRef::default()
    };
    let alpha_env = derive_receipt_type_env_for_scope(
        std::slice::from_ref(&ext_a),
        &base,
        &limits,
        &alpha_scope,
    )
    .map_err(|e| format!("alpha env failed: {e:?}"))?;
    assert!(
        alpha_env
            .record("retail.Sku")
            .and_then(|rec| rec.fields.get("acme__secret_amount"))
            .is_some()
    );
    let beta_env = derive_receipt_type_env_for_scope(
        std::slice::from_ref(&ext_a),
        &base,
        &limits,
        &beta_scope,
    )
    .map_err(|e| format!("beta env failed: {e:?}"))?;
    assert!(
        beta_env
            .record("retail.Sku")
            .and_then(|rec| rec.fields.get("acme__secret_amount"))
            .is_none()
    );

    // Receipt binds the scope-keyed type_env digest (changes when extension scope changes) and verifies round-trip.
    let to_rv = |a: &ArtifactEnvelope| -> Result<RuntimeValue, Box<dyn std::error::Error>> {
        Ok(RuntimeValue::from_json(serde_json::to_value(a)?)?)
    };
    let base_rv: Vec<RuntimeValue> = base.iter().map(to_rv).collect::<Result<_, _>>()?;
    let reg = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        registry: &reg,
        base: &base,
        tenant: None,
    };
    let checked_a = check_artifact_set_with_receipt(
        &ReceiptSources::new(&[to_rv(&ext_a)?], &base_rv)?,
        &ctx,
        &limits,
    )?;
    assert!(checked_a.receipt.admitted, "{:#?}", checked_a.diagnostics);
    let checked_b = check_artifact_set_with_receipt(
        &ReceiptSources::new(&[to_rv(&ext_b)?], &base_rv)?,
        &ctx,
        &limits,
    )?;
    assert!(checked_b.receipt.admitted, "{:#?}", checked_b.diagnostics);
    assert_ne!(
        checked_a.receipt.type_env, checked_b.receipt.type_env,
        "scope-bound receipt typeEnv digest must reflect extension scope"
    );
    let body_a = String::from_utf8(checked_a.receipt.canonical_json()?)?;
    let sha_a = checked_a.receipt.content_sha256()?;
    verify_admission_receipt(&body_a, &sha_a, &[ext_a], &base, &limits)?;
    Ok(())
}

fn check_entities_only(cands: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let reg = BTreeSet::<PropertyTypeName>::new();
    norn_checker::check_artifact_set_entity_model(
        cands,
        &EntityArtifactSetContext {
            registry: &reg,
            base,
            tenant: None,
        },
    )
}

#[test]
fn installed_base_invalid_object_supertype_edge_is_refused() -> TestResult {
    let (global, tenant_a) = (json!({}), json!({ "tenantId": "tenant_a" }));
    let super_a = envelope(
        "retail.SuperAlpha.entity",
        "entity",
        &tenant_a,
        &json!([]),
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.SuperAlpha",
                "properties": [{ "id": "code", "type": { "kind": "text" } }],
                "identity": [{ "name": "primary", "properties": ["code"] }]
            }
        }),
    )?;
    let middle_global = envelope(
        "retail.MiddleGlobal.entity",
        "entity",
        &global,
        &json!([]),
        &json!({ "entityVersion": "2.0", "objectType": { "name": "retail.MiddleGlobal", "extends": "retail.SuperAlpha", "properties": [] } }),
    )?;
    let child_a = envelope(
        "retail.ChildAlpha.entity",
        "entity",
        &tenant_a,
        &json!([]),
        &json!({ "entityVersion": "2.0", "objectType": { "name": "retail.ChildAlpha", "extends": "retail.MiddleGlobal", "properties": [] } }),
    )?;
    let findings = check_entities_only(&[child_a], &[super_a, middle_global]);
    assert!(
        errors_for(&findings, "retail.ChildAlpha.entity")
            .iter()
            .any(|d| d.code.as_str() == norn_checker::artifact_set::codes::INCOMPATIBLE_SCOPE),
        "narrower candidate must fail closed on installed broader->narrower edge: {findings:#?}"
    );
    Ok(())
}

#[test]
fn installed_base_invalid_interface_supertype_edge_is_refused() -> TestResult {
    let (global, tenant_a) = (json!({}), json!({ "tenantId": "tenant_a" }));
    let iface_a = envelope(
        "retail.IfaceAlpha.interface",
        "entity",
        &tenant_a,
        &json!([]),
        &json!({ "entityVersion": "2.0", "interface": { "name": "retail.IfaceAlpha", "properties": [{ "id": "code", "type": { "kind": "text" } }] } }),
    )?;
    let iface_mid_global = envelope(
        "retail.IfaceMidGlobal.interface",
        "entity",
        &global,
        &json!([]),
        &json!({ "entityVersion": "2.0", "interface": { "name": "retail.IfaceMidGlobal", "extends": ["retail.IfaceAlpha"], "properties": [] } }),
    )?;
    let impl_a = envelope(
        "retail.ImplAlpha.entity",
        "entity",
        &tenant_a,
        &json!([]),
        &json!({ "entityVersion": "2.0", "objectType": { "name": "retail.ImplAlpha", "implements": ["retail.IfaceMidGlobal"], "properties": [{ "id": "code", "type": { "kind": "text" } }] } }),
    )?;
    let findings = check_entities_only(&[impl_a], &[iface_a, iface_mid_global]);
    assert!(
        errors_for(&findings, "retail.ImplAlpha.entity")
            .iter()
            .any(|d| d.code.as_str() == norn_checker::artifact_set::codes::INCOMPATIBLE_SCOPE),
        "narrower candidate must fail closed on installed interface broader->narrower edge: {findings:#?}"
    );
    Ok(())
}
