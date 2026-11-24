//! Security #1376 regression suite (part 2): consumers outside the entity
//! model (laws, models, views, labels, mappings, defined concepts, derived
//! metrics, the definition catalog, flow source maps and admission receipts)
//! resolve only what their scope sees.

use std::collections::BTreeSet;

use norn_checker::artifact_set::codes::INCOMPATIBLE_SCOPE;
use norn_checker::entity_scopes::{
    SCOPED_TYPE_ENV_ID, ScopedView, check_entity_reference_scopes_all,
};
use norn_checker::metric::derive::derive_metric_space;
use norn_checker::{
    ArtifactRef, EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt,
    definition_catalog, validate_flow_source_map, verify_admission_receipt,
};
use norn_ir::{
    ArtifactEnvelope, FlowSourceMap, LawRoot, ResourceLimitProfile, RuntimeValue, ScopeRef,
};
use serde_json::{Value, json};

use super::extension_scope::{
    TestResult, assert_admitted, assert_refused, base_sku, check_set, envelope, has_scope_error,
    partner_entity, sku_extension,
};

type Built = Result<ArtifactEnvelope, Box<dyn std::error::Error>>;
type Builder = fn(&str, &Value) -> Built;

fn same_scope() -> Value {
    json!({ "orgId": "org_a", "businessUnitId": "bu_a" })
}

fn sku_dependency() -> Value {
    json!([{ "artifactId": "retail.Sku.entity", "kind": "entity", "version": "1.0.0", "required": true }])
}

/// A consumer reading `extension` (a candidate, then installed) is admitted
/// in the extension's scope or a narrower one and refused in a broader or a
/// sibling one.
fn assert_consumer_scopes(
    build: &dyn Fn(&str, &Value) -> Built,
    prefix: &str,
    extension: &ArtifactEnvelope,
    base: &[ArtifactEnvelope],
) -> TestResult {
    let cases = [
        ("broader", json!({ "orgId": "org_a" }), false),
        (
            "sibling",
            json!({ "orgId": "org_a", "businessUnitId": "bu_b" }),
            false,
        ),
        ("same", same_scope(), true),
        (
            "narrow",
            json!({ "orgId": "org_a", "businessUnitId": "bu_a", "workspaceId": "ws_child" }),
            true,
        ),
    ];
    for installed in [false, true] {
        let mut base = base.to_vec();
        if installed {
            base.push(extension.clone());
        }
        for (suffix, scope, admitted) in &cases {
            let id = format!("retail.{prefix}_{suffix}");
            let consumer = build(&id, scope)?;
            let candidates = if installed {
                vec![consumer]
            } else {
                vec![extension.clone(), consumer]
            };
            let findings = check_set(&candidates, &base);
            let context = format!("{prefix} {suffix}, extension installed: {installed}");
            if *admitted {
                assert_admitted(&findings, &id, &context);
            } else {
                assert_refused(&findings, &id, &context);
            }
        }
    }
    Ok(())
}

fn law(id: &str, scope: &Value) -> Built {
    envelope(
        id,
        "law",
        scope,
        &sku_dependency(),
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
}

fn model(id: &str, scope: &Value) -> Built {
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
            "outputs": [{ "id": "score", "type": { "kind": "decimal", "scale": 2 }, "nullable": false }],
            "trust": "opaque",
            "method": { "label": "linear" }
        }),
    )
}

fn view(id: &str, scope: &Value) -> Built {
    envelope(
        id,
        "derived",
        scope,
        &sku_dependency(),
        &json!({
            "viewVersion": "0.1",
            "viewId": id,
            "inputs": [{ "alias": "skus", "kind": "entity", "artifactId": "retail.Sku.entity", "version": "1.0.0" }],
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
                            { "name": "secret", "expression": "input.acme__secret_amount" }
                        ]
                    }
                }
            }
        }),
    )
}

fn labels(id: &str, scope: &Value) -> Built {
    envelope(
        id,
        "labels",
        scope,
        &json!([]),
        &json!({
            "labelsVersion": "0.1",
            "entries": [{
                "target": { "kind": "property", "objectType": "retail.Sku", "property": "acme__secret_tag" },
                "label": { "en": "Secret tag" }
            }]
        }),
    )
}

fn mapping(id: &str, scope: &Value) -> Built {
    let source_hash = ArtifactRef::of(&mapping_source_schema()?)?.sha256;
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
            "source": { "schemaId": "erp.source_schema", "version": "1.0.0", "sha256": source_hash },
            "interfaces": ["retail.CatalogItem.interface"],
            "maps": [{
                "id": "sku",
                "source": { "dataset": "Item" },
                "target": { "entity": "retail.MappedSku.entity", "interface": "retail.CatalogItem.interface" },
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
}

#[test]
fn consumers_resolve_only_extensions_their_scope_sees() -> TestResult {
    let base = [base_sku(&json!({}))?, partner_entity(&json!({}))?];
    let extension = sku_extension("acme.SkuExt.entity", &same_scope())?;
    let consumers: [(&str, Builder); 4] = [
        ("law", law),
        ("model", model),
        ("view", view),
        ("labels", labels),
    ];
    for (prefix, build) in consumers {
        assert_consumer_scopes(&build, prefix, &extension, &base)?;
    }
    let (interface, mapped, mapped_extension) = mapping_entities(&same_scope())?;
    let mapping_base = [interface, mapped, mapping_source_schema()?];
    assert_consumer_scopes(&mapping, "map", &mapped_extension, &mapping_base)
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
fn a_scoped_view_derives_catalog_environment_and_metrics_from_its_scope_alone() -> TestResult {
    let tenant = |id: &str| ScopeRef {
        tenant_id: Some(id.to_owned()),
        ..ScopeRef::default()
    };
    let sku = base_sku(&json!({}))?;
    let pool = vec![
        sku.clone(),
        partner_entity(&json!({}))?,
        envelope(
            "acme.SkuTimed.entity",
            "entity",
            &json!({ "tenantId": "tenant_a" }),
            &json!([]),
            &json!({ "entityVersion": "2.0", "objectTypeExtension": {
                "namespace": "acme",
                "target": "retail.Sku",
                "properties": [
                    { "id": "acme__secret_amount", "type": { "kind": "decimal", "scale": 2, "additivity": { "kind": "additive" } },
                      "constraints": { "nullable": false }, "default": "0.00" },
                    { "id": "acme__seen_on", "type": { "kind": "date" }, "constraints": { "nullable": true } }
                ]
            } }),
        )?,
    ];
    let registry = BTreeSet::new();
    let mixed = ScopedView::uniform(&pool, &registry).into_catalog();
    let mixed_sku = mixed
        .object_type_named("retail.Sku")
        .and_then(|sku| sku.ok());
    assert!(mixed_sku.is_some_and(|sku| sku.property("acme__secret_amount").is_none()));
    let limits = ResourceLimitProfile::norn_limits_v1();
    for (scope, sees) in [
        (tenant("tenant_a"), true),
        (tenant("tenant_b"), false),
        (ScopeRef::default(), false),
    ] {
        let view = ScopedView::bound(&scope, &pool, &[], &registry);
        let catalog_sku = view
            .catalog()
            .object_type_named("retail.Sku")
            .ok_or("retail.Sku")?
            .map_err(|finding| format!("{finding:?}"))?;
        assert_eq!(
            catalog_sku.property("acme__secret_amount").is_some(),
            sees,
            "{scope:?}"
        );
        let env = view
            .receipt_env(&limits)
            .map_err(|errors| format!("{errors:?}"))?;
        let record = env.record("retail.Sku").ok_or("retail.Sku record")?;
        assert_eq!(
            record.fields.contains_key("acme__secret_amount"),
            sees,
            "{scope:?}"
        );
        let space = derive_metric_space(&view, &LawRoot::Cell("global".to_owned()))
            .map_err(|findings| format!("{findings:?}"))?;
        let derived = space
            .candidates()
            .iter()
            .any(|candidate| candidate.definition.name.contains("acme__secret_amount"));
        assert_eq!(derived, sees, "{scope:?}");
    }
    let projected = definition_catalog::project(&pool, &[], &registry)?;
    assert!(!projected.properties.iter().any(|row| {
        row.at.artifact_id == "retail.Sku.entity"
            && matches!(&row.at.member, norn_ir::definition_catalog::Member::Property { property, .. }
                if property == "acme__secret_amount")
    }));
    let flow_map: FlowSourceMap = serde_json::from_value(json!({
        "format": "norn-flow-source-map/1",
        "flows": [{
            "flowId": "sku-flow",
            "flowVersion": "1.0.0",
            "sourcePath": "flows/sku.ts",
            "artifactRefs": [{ "artifactId": "retail.Sku.entity", "version": "1.0.0", "sha256": ArtifactRef::of(&sku)?.sha256 }],
            "steps": [{ "id": "s1", "kind": "perform", "artifactIds": ["retail.Sku.entity"], "stateIds": ["draft"], "decisionRowIds": [] }],
            "edges": []
        }]
    }))?;
    assert_eq!(validate_flow_source_map(&flow_map, &pool, &[]), []);
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
fn receipts_commit_to_each_scope_and_still_verify_whole_set_receipts() -> TestResult {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let base = vec![base_sku(&json!({}))?, partner_entity(&json!({}))?];
    let alpha = sku_extension("acme.SkuExt.entity", &json!({ "workspaceId": "ws_alpha" }))?;
    let beta = sku_extension("acme.SkuExt.entity", &json!({ "workspaceId": "ws_beta" }))?;
    let to_runtime =
        |artifact: &ArtifactEnvelope| -> Result<RuntimeValue, Box<dyn std::error::Error>> {
            Ok(RuntimeValue::from_json(serde_json::to_value(artifact)?)?)
        };
    let base_values: Vec<RuntimeValue> = base.iter().map(to_runtime).collect::<Result<_, _>>()?;
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &base,
        tenant: None,
    };
    let admit = |extension: &ArtifactEnvelope| -> Result<_, Box<dyn std::error::Error>> {
        let sources = ReceiptSources::new(&[to_runtime(extension)?], &base_values)?;
        let checked = check_artifact_set_with_receipt(&sources, &context, &limits)?;
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        Ok(checked.receipt)
    };
    let (receipt_alpha, receipt_beta) = (admit(&alpha)?, admit(&beta)?);
    let pinned = receipt_alpha.type_env.clone().ok_or("type environment")?;
    assert_eq!(pinned.id, SCOPED_TYPE_ENV_ID);
    assert_ne!(Some(pinned), receipt_beta.type_env);
    let verify = |receipt: &norn_checker::CheckReceipt| -> TestResult {
        let body = String::from_utf8(receipt.canonical_json()?)?;
        verify_admission_receipt(
            &body,
            &receipt.content_sha256()?,
            std::slice::from_ref(&alpha),
            &base,
            &limits,
        )?;
        Ok(())
    };
    verify(&receipt_alpha)?;
    let alpha_scope = ScopeRef {
        workspace_id: Some("ws_alpha".to_owned()),
        ..ScopeRef::default()
    };
    let whole_set = ScopedView::bound(&alpha_scope, std::slice::from_ref(&alpha), &base, &registry)
        .receipt_env(&limits)
        .map_err(|errors| format!("{errors:?}"))?
        .reference()?;
    let mut historical = receipt_alpha;
    historical.type_env = Some(whole_set);
    verify(&historical)?;
    let plain = check_artifact_set_with_receipt(
        &ReceiptSources::new(&base_values, &[])?,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
        &limits,
    )?;
    assert_eq!(
        plain.receipt.type_env.map(|reference| reference.id),
        Some(norn_expr::env::TYPE_ENV_ID.to_owned())
    );
    Ok(())
}

#[test]
fn link_targets_are_checked_in_the_shared_type_and_interface_name_space() -> TestResult {
    let interface = |scope: &Value| {
        envelope(
            "retail.Secret.interface",
            "entity",
            scope,
            &json!([]),
            &json!({ "entityVersion": "2.0", "interface": {
                "name": "retail.Secret",
                "properties": [{ "id": "code", "type": { "kind": "text" } }]
            } }),
        )
    };
    let link = |id: &str| json!([{ "id": id, "target": "retail.Secret", "cardinality": { "to": "one", "from": "many" } }]);
    let holders = [
        envelope(
            "retail.Holder.entity",
            "entity",
            &json!({}),
            &json!([]),
            &json!({ "entityVersion": "2.0", "objectType": {
            "name": "retail.Holder",
            "properties": [{ "id": "code", "type": { "kind": "text" } }],
            "identity": [{ "name": "primary", "properties": ["code"] }],
            "links": link("secret")
        } }),
        )?,
        envelope(
            "acme.SkuSecret.entity",
            "entity",
            &json!({}),
            &json!([]),
            &json!({ "entityVersion": "2.0", "objectTypeExtension": {
            "namespace": "acme",
            "target": "retail.Sku",
            "links": link("acme__secret")
        } }),
        )?,
        envelope(
            "retail.Holding.interface",
            "entity",
            &json!({}),
            &json!([]),
            &json!({ "entityVersion": "2.0", "interface": {
            "name": "retail.Holding",
            "properties": [],
            "links": link("secret")
        } }),
        )?,
    ];
    for (scope, refused) in [
        (json!({ "tenantId": "tenant_a" }), true),
        (json!({}), false),
    ] {
        let base = [
            base_sku(&json!({}))?,
            partner_entity(&json!({}))?,
            interface(&scope)?,
        ];
        for holder in &holders {
            let id = holder.artifact_id.as_str();
            let findings = check_set(std::slice::from_ref(holder), &base);
            assert_eq!(
                has_scope_error(&findings, id),
                refused,
                "{id} at {scope}: {findings:#?}"
            );
            let mut installed = base.to_vec();
            installed.push(holder.clone());
            let projected = check_entity_reference_scopes_all(&installed)
                .iter()
                .any(|finding| {
                    finding.artifact_id.as_deref() == Some(id)
                        && finding.code.as_str() == INCOMPATIBLE_SCOPE
                });
            assert_eq!(
                projected, refused,
                "{id} at {scope}: kernel projection check"
            );
        }
    }
    Ok(())
}

#[test]
fn installed_supertype_and_interface_edges_into_a_narrower_scope_are_refused() -> TestResult {
    let (global, tenant_a) = (json!({}), json!({ "tenantId": "tenant_a" }));
    let code = json!([{ "id": "code", "type": { "kind": "text" } }]);
    let object = |id: &str, scope: &Value, body: Value| {
        envelope(
            id,
            "entity",
            scope,
            &json!([]),
            &json!({ "entityVersion": "2.0", "objectType": body }),
        )
    };
    let interface = |id: &str, scope: &Value, body: Value| {
        envelope(
            id,
            "entity",
            scope,
            &json!([]),
            &json!({ "entityVersion": "2.0", "interface": body }),
        )
    };
    let cases = [
        (
            object(
                "retail.ChildAlpha.entity",
                &tenant_a,
                json!({ "name": "retail.ChildAlpha", "extends": "retail.MiddleGlobal", "properties": [] }),
            )?,
            vec![
                object(
                    "retail.SuperAlpha.entity",
                    &tenant_a,
                    json!({ "name": "retail.SuperAlpha", "properties": code, "identity": [{ "name": "primary", "properties": ["code"] }] }),
                )?,
                object(
                    "retail.MiddleGlobal.entity",
                    &global,
                    json!({ "name": "retail.MiddleGlobal", "extends": "retail.SuperAlpha", "properties": [] }),
                )?,
            ],
        ),
        (
            object(
                "retail.ImplAlpha.entity",
                &tenant_a,
                json!({ "name": "retail.ImplAlpha", "implements": ["retail.IfaceMidGlobal"], "properties": code }),
            )?,
            vec![
                interface(
                    "retail.IfaceAlpha.interface",
                    &tenant_a,
                    json!({ "name": "retail.IfaceAlpha", "properties": code }),
                )?,
                interface(
                    "retail.IfaceMidGlobal.interface",
                    &global,
                    json!({ "name": "retail.IfaceMidGlobal", "extends": ["retail.IfaceAlpha"], "properties": [] }),
                )?,
            ],
        ),
    ];
    for (candidate, base) in cases {
        let findings =
            super::extension_scope::check_entities(std::slice::from_ref(&candidate), &base);
        assert!(
            has_scope_error(&findings, &candidate.artifact_id),
            "{findings:#?}"
        );
    }
    Ok(())
}
