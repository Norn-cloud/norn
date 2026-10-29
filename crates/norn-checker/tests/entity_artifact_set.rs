#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! #2844: `EntityIR` v2 definition forms (interface, extension, property type
//! catalog) and the artifact-set check that runs `check_entity_model` over
//! `entity` artifacts.

use std::collections::BTreeSet;

use norn_checker::entity_artifacts::codes::LAYER_SCOPE_MISMATCH;
use norn_checker::object_types::codes as object_codes;
use norn_checker::property_types::codes as property_codes;
use norn_checker::{
    EntityArtifactSetContext, TenantLayer, check_artifact, check_artifact_set_entity_model,
    check_entity_artifacts, validate_artifact,
};
use norn_ir::{ArtifactEnvelope, ExtensionNamespace, PropertyTypeName};
use serde_json::{Value, json};

const CATALOG: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/commerce.property-types.entity.json");
const INTERFACE: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/party.interface.entity.json");
const SUPPLIER: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/purchasing-supplier.entity.json");
const SKU: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json");
const EXTENSION: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/acme-sku.extension.entity.json");
const UNKNOWN_SUPERTYPE: &str = include_str!(
    "../../../fixtures/ir/entity-v2/artifact-set/invalid/unknown-supertype.entity.json"
);
const UNKNOWN_LINK_TARGET: &str = include_str!(
    "../../../fixtures/ir/entity-v2/artifact-set/invalid/unknown-link-target.entity.json"
);
const CONFLICTING_INVERSE: &str = include_str!(
    "../../../fixtures/ir/entity-v2/artifact-set/invalid/conflicting-inverse.entity.json"
);

const ENTITY: &str = "entity.validation.invalid_payload";

fn json_of(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture should parse")
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("envelope should decode leniently")
}

fn registry_layer() -> Vec<Value> {
    [CATALOG, INTERFACE, SUPPLIER, SKU]
        .into_iter()
        .map(json_of)
        .collect()
}

fn envelopes(values: &[Value]) -> Vec<ArtifactEnvelope> {
    values.iter().cloned().map(envelope).collect()
}

fn acme() -> ExtensionNamespace {
    ExtensionNamespace("acme".to_owned())
}

/// The tenant id every fixture of the `acme` tenant layer is scoped to.
const ACME_TENANT: &str = "tenant_acme";

/// `(artifact id, code, pointer)` of every set finding; a tenant layer is
/// the `tenant_acme` tenant under `namespace`.
fn check_set(
    artifacts: &[Value],
    base: &[Value],
    namespace: Option<&ExtensionNamespace>,
) -> Vec<(String, String, String)> {
    check_layer(
        artifacts,
        base,
        namespace.map(|namespace| TenantLayer {
            tenant_id: ACME_TENANT,
            namespace,
        }),
    )
}

fn located(finding: norn_checker::Diagnostic) -> (String, String, String) {
    (
        finding.artifact_id.unwrap_or_default(),
        finding.code.as_str().to_owned(),
        finding.span.map(|span| span.pointer).unwrap_or_default(),
    )
}

/// `(artifact id, code, pointer)` of every finding of the combined entry
/// point ([`check_entity_artifacts`]); a tenant layer as in [`check_set`].
fn check_combined(
    artifacts: &[Value],
    base: &[Value],
    namespace: Option<&ExtensionNamespace>,
) -> Vec<(String, String, String)> {
    let registry: BTreeSet<PropertyTypeName> = BTreeSet::new();
    let base = envelopes(base);
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &base,
        tenant: namespace.map(|namespace| TenantLayer {
            tenant_id: ACME_TENANT,
            namespace,
        }),
    };
    check_entity_artifacts(&envelopes(artifacts), &context)
        .into_iter()
        .map(located)
        .collect()
}

/// `(artifact id, code, pointer)` of every set finding.
fn check_layer(
    artifacts: &[Value],
    base: &[Value],
    tenant: Option<TenantLayer<'_>>,
) -> Vec<(String, String, String)> {
    let registry: BTreeSet<PropertyTypeName> = BTreeSet::new();
    let base = envelopes(base);
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &base,
        tenant,
    };
    check_artifact_set_entity_model(&envelopes(artifacts), &context)
        .into_iter()
        .map(|finding| {
            (
                finding.artifact_id.unwrap_or_default(),
                finding.code.as_str().to_owned(),
                finding.span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn one(found: &[(String, String, String)], artifact: &str, code: &str, pointer: &str) {
    assert_eq!(
        found,
        [(artifact.to_owned(), code.to_owned(), pointer.to_owned())],
        "expected exactly {code} on {artifact} at {pointer}"
    );
}

/// `(code, path)` of every per-artifact finding.
fn artifact_findings(value: Value) -> Vec<(String, String)> {
    validate_artifact(&envelope(value))
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.path.unwrap_or_default()))
        .collect()
}

#[test]
fn every_fixture_artifact_passes_the_per_artifact_check() {
    for source in [
        CATALOG,
        INTERFACE,
        SUPPLIER,
        SKU,
        EXTENSION,
        UNKNOWN_SUPERTYPE,
        UNKNOWN_LINK_TARGET,
        CONFLICTING_INVERSE,
    ] {
        let artifact = envelope(json_of(source));
        assert_eq!(check_artifact(&artifact), [], "{}", artifact.artifact_id);
    }
}

#[test]
fn the_registry_layer_and_the_tenant_layer_check_clean() {
    let registry = registry_layer();
    assert_eq!(check_set(&registry, &[], None), []);
    assert_eq!(
        check_set(&[json_of(EXTENSION)], &registry, Some(&acme())),
        []
    );
    // Order within the set does not matter.
    let mut reversed = registry.clone();
    reversed.reverse();
    assert_eq!(check_set(&reversed, &[], None), []);
    // A layer built on part of the registry layer resolves the rest from it.
    assert_eq!(
        check_set(&registry[2..], &registry[..2], None),
        [],
        "object types resolve the catalog and interface from the base layer"
    );
}

#[test]
fn an_unknown_supertype_is_reported_on_its_artifact() {
    let mut set = registry_layer();
    set.push(json_of(UNKNOWN_SUPERTYPE));
    one(
        &check_set(&set, &[], None),
        "retail.SeasonalSku.entity.v2",
        object_codes::UNKNOWN_SUPERTYPE,
        "/payload/objectType/extends",
    );
}

#[test]
fn an_unknown_link_target_is_reported_at_the_link_target() {
    let mut set = registry_layer();
    set.push(json_of(UNKNOWN_LINK_TARGET));
    one(
        &check_set(&set, &[], None),
        "retail.Bundle.entity.v2",
        object_codes::UNKNOWN_LINK_TARGET,
        "/payload/objectType/links/0/target",
    );
}

#[test]
fn a_conflicting_inverse_role_is_reported_on_the_link_that_declares_it() {
    let mut set = registry_layer();
    set.push(json_of(CONFLICTING_INVERSE));
    one(
        &check_set(&set, &[], None),
        "purchasing.PurchaseOrderLine.entity.v2",
        object_codes::MEMBER_ID_CONFLICT,
        "/payload/objectType/links/0/inverse",
    );
}

#[test]
fn refs_resolve_across_catalog_artifacts_and_the_base_layer() {
    // Without the catalog, each `ref` is unknown on its own artifact.
    let registry = registry_layer();
    let found = check_set(&registry[1..], &[], None);
    assert_eq!(
        found,
        [
            (
                "purchasing.Supplier.entity.v2".to_owned(),
                property_codes::UNKNOWN_REF.to_owned(),
                "/payload/objectType/properties/1/type/name".to_owned()
            ),
            (
                "retail.Sku.entity.v2".to_owned(),
                property_codes::UNKNOWN_REF.to_owned(),
                "/payload/objectType/properties/0/type/name".to_owned()
            ),
        ]
    );

    // Two catalogs may not define one name; the later one is reported.
    let mut second = json_of(CATALOG);
    second["artifactId"] = json!("commerce.property_types.entity.v2.copy");
    second["payload"]["propertyTypes"] = json!([second["payload"]["propertyTypes"][1].clone()]);
    let mut set = registry.clone();
    set.push(second.clone());
    one(
        &check_set(&set, &[], None),
        "commerce.property_types.entity.v2.copy",
        property_codes::DUPLICATE_NAME,
        "/payload/propertyTypes/0/name",
    );
    let message = check_artifact_set_entity_model(
        &envelopes(&set),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )[0]
    .message
    .clone();
    assert_eq!(
        message,
        "property type \"party.display_name\" is already defined by artifact \"commerce.property_types.entity.v2\""
    );
    // ...nor redefine a base-layer name.
    one(
        &check_set(&[second], &registry, None),
        "commerce.property_types.entity.v2.copy",
        property_codes::DUPLICATE_NAME,
        "/payload/propertyTypes/0/name",
    );

    // A catalog `ref` into another catalog artifact resolves.
    let mut alias = json_of(CATALOG);
    alias["artifactId"] = json!("commerce.aliases.entity.v2");
    alias["payload"]["propertyTypes"] = json!([
        { "name": "commerce.vendor_sku_code", "type": { "kind": "ref", "name": "commerce.sku_code" } }
    ]);
    let mut set = registry;
    set.push(alias.clone());
    assert_eq!(check_set(&set, &[], None), []);
    one(
        &check_set(&[alias], &[], None),
        "commerce.aliases.entity.v2",
        property_codes::UNKNOWN_REF,
        "/payload/propertyTypes/0/type/name",
    );
}

#[test]
fn interfaces_and_extensions_resolve_across_artifacts() {
    // An object type implementing an interface that no artifact defines.
    let registry = registry_layer();
    let without_interface: Vec<Value> = registry
        .iter()
        .filter(|value| value["payload"].get("interface").is_none())
        .cloned()
        .collect();
    one(
        &check_set(&without_interface, &[], None),
        "purchasing.Supplier.entity.v2",
        object_codes::UNKNOWN_INTERFACE,
        "/payload/objectType/implements/0",
    );

    // An interface requirement the implementing type does not meet.
    let mut interface = json_of(INTERFACE);
    interface["payload"]["interface"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({ "id": "tax_id", "type": { "kind": "text" } }));
    let set = [json_of(CATALOG), interface, json_of(SUPPLIER), json_of(SKU)];
    one(
        &check_set(&set, &[], None),
        "purchasing.Supplier.entity.v2",
        object_codes::MISSING_INTERFACE_PROPERTY,
        "/payload/objectType/implements/0",
    );

    // An extension whose target is not installed.
    let mut extension = json_of(EXTENSION);
    extension["payload"]["objectTypeExtension"]["target"] = json!("retail.Item");
    one(
        &check_set(&[extension], &registry, Some(&acme())),
        "acme.retail.Sku.extension.v2",
        object_codes::UNKNOWN_EXTENSION_TARGET,
        "/payload/objectTypeExtension/target",
    );

    // An extension member that is not in the tenant namespace.
    let mut extension = json_of(EXTENSION);
    extension["payload"]["objectTypeExtension"]["properties"][0]["id"] = json!("shelf_label");
    one(
        &check_set(&[extension], &registry, Some(&acme())),
        "acme.retail.Sku.extension.v2",
        object_codes::UNNAMESPACED_MEMBER,
        "/payload/objectTypeExtension/properties/0/id",
    );

    // An extension in another tenant's namespace.
    let other = ExtensionNamespace("globex".to_owned());
    one(
        &check_set(&[json_of(EXTENSION)], &registry, Some(&other)),
        "acme.retail.Sku.extension.v2",
        object_codes::EXTENSION_NAMESPACE_MISMATCH,
        "/payload/objectTypeExtension/namespace",
    );
}

#[test]
fn layer_scope_mismatch_is_reported_at_the_scope() {
    let registry = registry_layer();

    // A tenant-scoped artifact in a registry layer.
    let mut set = registry.clone();
    set[1]["scope"]["tenantId"] = json!("tenant_acme");
    one(
        &check_set(&set, &[], None),
        "party.Party.interface.v2",
        LAYER_SCOPE_MISMATCH,
        "/scope/tenantId",
    );

    // A tenant-layer artifact without a tenant.
    let mut extension = json_of(EXTENSION);
    extension["scope"]
        .as_object_mut()
        .expect("scope")
        .remove("tenantId");
    one(
        &check_set(&[extension], &registry, Some(&acme())),
        "acme.retail.Sku.extension.v2",
        LAYER_SCOPE_MISMATCH,
        "/scope",
    );

    // Two tenants in one tenant layer.
    let mut other = json_of(EXTENSION);
    other["artifactId"] = json!("acme.retail.Sku.extension.v2.other");
    other["scope"]["tenantId"] = json!("tenant_globex");
    other["payload"]["objectTypeExtension"]["properties"][0]["id"] = json!("acme__aisle");
    one(
        &check_set(&[json_of(EXTENSION), other], &registry, Some(&acme())),
        "acme.retail.Sku.extension.v2.other",
        LAYER_SCOPE_MISMATCH,
        "/scope/tenantId",
    );

    // A whole layer scoped uniformly to the wrong tenant: every artifact is
    // compared against the layer's tenant id, not against the first one.
    let mut first = json_of(EXTENSION);
    first["scope"]["tenantId"] = json!("tenant_globex");
    let mut second = first.clone();
    second["artifactId"] = json!("acme.retail.Sku.extension.v2.other");
    second["payload"]["objectTypeExtension"]["properties"][0]["id"] = json!("acme__aisle");
    let namespace = acme();
    assert_eq!(
        check_layer(
            &[first, second],
            &registry,
            Some(TenantLayer {
                tenant_id: ACME_TENANT,
                namespace: &namespace,
            }),
        ),
        [
            (
                "acme.retail.Sku.extension.v2".to_owned(),
                LAYER_SCOPE_MISMATCH.to_owned(),
                "/scope/tenantId".to_owned()
            ),
            (
                "acme.retail.Sku.extension.v2.other".to_owned(),
                LAYER_SCOPE_MISMATCH.to_owned(),
                "/scope/tenantId".to_owned()
            ),
        ]
    );

    // Base-layer scopes are not re-checked, and v1 artifacts are not part of
    // the v2 model.
    let v1 = json_of(include_str!("../../../fixtures/ir/retail-sku.entity.json"));
    let mut tenant_scoped_v1 = v1;
    tenant_scoped_v1["scope"]["tenantId"] = json!("tenant_acme");
    assert_eq!(check_set(&[tenant_scoped_v1], &registry, None), []);
}

#[test]
fn the_set_check_is_fail_closed_alone_and_the_combined_check_reports_once() {
    // A duplicate property id: the per-artifact check reports it, the set
    // check alone reports it too, and the combined entry point once.
    let mut sku = json_of(SKU);
    sku["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({ "id": "title", "type": { "kind": "text" } }));
    assert_eq!(
        artifact_findings(sku.clone()),
        [(
            object_codes::DUPLICATE_PROPERTY_ID.to_owned(),
            "payload.objectType.properties[2].id".to_owned()
        )]
    );
    let mut set = registry_layer();
    set[3] = sku;
    let duplicate = (
        "retail.Sku.entity.v2".to_owned(),
        object_codes::DUPLICATE_PROPERTY_ID.to_owned(),
        "/payload/objectType/properties/2/id".to_owned(),
    );
    assert_eq!(check_set(&set, &[], None), std::slice::from_ref(&duplicate));
    assert_eq!(check_combined(&set, &[], None), [duplicate]);

    // An EntityIR v2 artifact that does not decode fails the set alone.
    let mut broken = json_of(SUPPLIER);
    broken["payload"]["objectType"]["properties"] = json!("nope");
    let mut set = registry_layer();
    set[2] = broken;
    let found = check_set(&set, &[], None);
    assert!(
        found.contains(&(
            "purchasing.Supplier.entity.v2".to_owned(),
            ENTITY.to_owned(),
            "/payload".to_owned()
        )),
        "{found:?}"
    );

    // A v2 payload with no definition member: the set check alone reports
    // it (no suppression), and the combined entry point reports it exactly
    // once, though the per-artifact check reports the identical finding.
    let mut empty = json_of(SKU);
    empty["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("objectType");
    let mut set = registry_layer();
    set[3] = empty.clone();
    let key = (
        "retail.Sku.entity.v2".to_owned(),
        ENTITY.to_owned(),
        "/payload".to_owned(),
    );
    let per_artifact: Vec<(String, String, String)> = check_artifact(&envelope(empty))
        .into_iter()
        .map(located)
        .collect();
    assert_eq!(
        per_artifact,
        std::slice::from_ref(&key),
        "per-artifact finding"
    );
    assert_eq!(
        check_set(&set, &[], None),
        std::slice::from_ref(&key),
        "set check alone"
    );
    assert_eq!(check_combined(&set, &[], None), [key], "combined");
}

#[test]
fn the_combined_entry_point_checks_the_fixture_layers() {
    let registry = registry_layer();
    assert_eq!(check_combined(&registry, &[], None), []);
    assert_eq!(
        check_combined(&[json_of(EXTENSION)], &registry, Some(&acme())),
        []
    );
    let mut set = registry;
    set.push(json_of(UNKNOWN_LINK_TARGET));
    one(
        &check_combined(&set, &[], None),
        "retail.Bundle.entity.v2",
        object_codes::UNKNOWN_LINK_TARGET,
        "/payload/objectType/links/0/target",
    );
}

#[test]
fn a_v2_entity_carries_exactly_one_definition() {
    // Two definitions: the second is reported.
    let mut artifact = json_of(SKU);
    artifact["payload"]["interface"] = json_of(INTERFACE)["payload"]["interface"].clone();
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.interface".to_owned())]
    );

    // None.
    let mut artifact = json_of(SKU);
    artifact["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("objectType");
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload".to_owned())]
    );

    // A mutation authority belongs to an object type only.
    let mut artifact = json_of(INTERFACE);
    artifact["payload"]["mutationAuthority"] = json!({ "kind": "generic_action" });
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.mutationAuthority".to_owned())]
    );

    // Each form's member has its shape.
    let mut artifact = json_of(CATALOG);
    artifact["payload"]["propertyTypes"] = json!([]);
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.propertyTypes".to_owned())]
    );
    let mut artifact = json_of(CATALOG);
    artifact["payload"]["propertyTypes"] = json!({});
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.propertyTypes".to_owned())]
    );
    let mut artifact = json_of(INTERFACE);
    artifact["payload"]["interface"] = json!([]);
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.interface".to_owned())]
    );
}

#[test]
fn new_forms_decode_with_located_failures_and_run_their_local_checks() {
    // Decode failures are located at the member or element.
    let mut artifact = json_of(CATALOG);
    artifact["payload"]["propertyTypes"][1]["type"] = json!({ "kind": "float" });
    assert_eq!(
        artifact_findings(artifact),
        [(ENTITY.to_owned(), "payload.propertyTypes[1]".to_owned())]
    );
    let mut artifact = json_of(INTERFACE);
    artifact["payload"]["interface"]["properties"][0]["default"] = json!("x");
    assert_eq!(
        artifact_findings(artifact),
        [(
            ENTITY.to_owned(),
            "payload.interface.properties[0]".to_owned()
        )]
    );
    let mut artifact = json_of(EXTENSION);
    artifact["payload"]["objectTypeExtension"]["description"] = Value::Null;
    assert_eq!(
        artifact_findings(artifact),
        [(
            ENTITY.to_owned(),
            "payload.objectTypeExtension.description".to_owned()
        )]
    );

    // Per-definition rules run on each form.
    let mut artifact = json_of(CATALOG);
    artifact["payload"]["propertyTypes"][1]["name"] = json!("commerce.sku_code");
    assert_eq!(
        artifact_findings(artifact),
        [(
            property_codes::DUPLICATE_NAME.to_owned(),
            "payload.propertyTypes[1].name".to_owned()
        )]
    );
    let mut artifact = json_of(INTERFACE);
    artifact["payload"]["interface"]["name"] = json!("Party");
    assert_eq!(
        artifact_findings(artifact),
        [(
            "interface.validation.invalid_name".to_owned(),
            "payload.interface.name".to_owned()
        )]
    );
    let mut artifact = json_of(EXTENSION);
    artifact["payload"]["objectTypeExtension"]["namespace"] = json!("Acme");
    assert!(
        artifact_findings(artifact).contains(&(
            object_codes::INVALID_EXTENSION_NAMESPACE.to_owned(),
            "payload.objectTypeExtension.namespace".to_owned()
        )),
        "invalid extension namespace is a per-artifact finding"
    );

    // A `ref` to another catalog is not a per-artifact finding.
    let mut artifact = json_of(CATALOG);
    artifact["payload"]["propertyTypes"][1]["type"] =
        json!({ "kind": "ref", "name": "party.name" });
    artifact["payload"]["propertyTypes"][1]["constraints"] = json!({});
    assert_eq!(artifact_findings(artifact), []);
}

#[test]
fn v2_definitions_are_closed_at_every_level() {
    // T1-ONT-2 (#2515): an unknown member anywhere in a v2 definition is a
    // located decode failure, never silently dropped.
    let cases: [(&str, &[&str]); 4] = [
        (SKU, &["objectType"]),
        (SKU, &["objectType", "links", "0"]),
        (INTERFACE, &["interface"]),
        (EXTENSION, &["objectTypeExtension"]),
    ];
    for (source, path) in cases {
        let mut artifact = json_of(source);
        let mut node = &mut artifact["payload"];
        for segment in path {
            node = match segment.parse::<usize>() {
                Ok(index) => &mut node[index],
                Err(_) => &mut node[*segment],
            };
        }
        node["unexpected"] = json!(true);
        let found = artifact_findings(artifact);
        assert_eq!(found.len(), 1, "{path:?}: {found:?}");
        assert_eq!(found[0].0, ENTITY, "{path:?}");
    }
    // The property shape inside the object type is closed too.
    let mut artifact = json_of(SKU);
    artifact["payload"]["objectType"]["properties"][0]["unexpected"] = json!(true);
    assert_eq!(
        artifact_findings(artifact),
        [(
            ENTITY.to_owned(),
            "payload.objectType.properties[0]".to_owned()
        )]
    );
}
