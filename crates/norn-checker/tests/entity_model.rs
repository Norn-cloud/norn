#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! `check_entity_model` (T1-ONT-1c, #2513; T1-ONT-1d, #2514): the fixture
//! model (an interface implemented by two types, a subtype, and a tenant
//! layer with an extension and a tenant subtype; 1:1, 1:N, and N:M links with
//! inverse roles and link properties, a required interface link, an
//! invariant, and a tenant extension link) is valid, and every whole-model
//! diagnostic code has a negative case.

use std::collections::BTreeSet;

use norn_checker::{
    Diagnostic, EntityContext, EntityModel, check_entity_model, check_object_type_defs,
    effective_object_types, interfaces::codes as interface_codes, object_types::codes,
};
use norn_ir::{
    ExtensionNamespace, InterfaceDef, ObjectTypeDef, ObjectTypeExtension, PropertyTypeDef,
    PropertyTypeName,
};

#[path = "support/model_codes.rs"]
mod model_codes;
use model_codes::MODEL_CODES;

macro_rules! fixture {
    ($dir:literal, $name:literal) => {
        include_str!(concat!("../../../fixtures/ir/entity-v2/", $dir, "/", $name))
    };
}

const CATALOG: [&str; 3] = [
    fixture!("property-types", "text.property-type.json"),
    fixture!("property-types", "boolean.property-type.json"),
    fixture!("property-types", "ref.property-type.json"),
];

fn registry() -> BTreeSet<PropertyTypeName> {
    BTreeSet::from([PropertyTypeName("iso4217.currency_code".to_owned())])
}

fn catalog() -> Vec<PropertyTypeDef> {
    CATALOG
        .iter()
        .map(|source| serde_json::from_str(source).expect("property type fixture decodes"))
        .collect()
}

/// One layer's definitions, decoded.
#[derive(Default)]
struct Layer {
    interfaces: Vec<InterfaceDef>,
    object_types: Vec<ObjectTypeDef>,
    extensions: Vec<ObjectTypeExtension>,
}

impl Layer {
    /// From `{ "interfaces": [...], "objectTypes": [...], "extensions": [...] }`
    /// (each member optional).
    fn from_json(value: &serde_json::Value) -> Self {
        let array = |member: &str| {
            value
                .get(member)
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]))
        };
        Self {
            interfaces: serde_json::from_value(array("interfaces")).expect("interfaces decode"),
            object_types: serde_json::from_value(array("objectTypes"))
                .expect("object types decode"),
            extensions: serde_json::from_value(array("extensions")).expect("extensions decode"),
        }
    }

    fn model(&self) -> EntityModel<'_> {
        EntityModel {
            interfaces: &self.interfaces,
            object_types: &self.object_types,
            extensions: &self.extensions,
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(source: &str) -> T {
    serde_json::from_str(source).expect("fixture decodes")
}

/// The registry-side fixture layer: `party.Party`, its two implementors, and
/// a subtype; `commerce.Order` (a required link) and the linked types.
fn base_layer() -> Layer {
    Layer {
        interfaces: vec![
            parse(fixture!("interfaces", "party.interface.json")),
            parse(fixture!("interfaces", "order.interface.json")),
        ],
        object_types: vec![
            parse(fixture!("object-types", "customer.object-type.json")),
            parse(fixture!("object-types", "employee.object-type.json")),
            parse(fixture!("object-types", "key-account.object-type.json")),
            parse(fixture!("object-types", "sales-order.object-type.json")),
            parse(fixture!("object-types", "billing-profile.object-type.json")),
            parse(fixture!("object-types", "project.object-type.json")),
            parse(fixture!("object-types", "meta-table.object-type.json")),
        ],
        extensions: Vec::new(),
    }
}

/// Tenant `acme`'s referral link on `crm.Customer`, with its inverse role.
fn tenant_link_layer() -> Layer {
    Layer {
        extensions: vec![parse(fixture!(
            "extensions",
            "acme-customer-referrals.extension.json"
        ))],
        ..Layer::default()
    }
}

/// The tenant `acme` fixture layer: an extension of `crm.Customer` and a
/// tenant subtype.
fn tenant_layer() -> Layer {
    Layer {
        interfaces: Vec::new(),
        object_types: vec![parse(fixture!(
            "object-types",
            "acme-wholesale-customer.object-type.json"
        ))],
        extensions: vec![parse(fixture!(
            "extensions",
            "acme-customer.extension.json"
        ))],
    }
}

fn run(model: &Layer, base: &Layer, tenant: Option<&str>) -> Vec<(String, String)> {
    let catalog = catalog();
    let registry = registry();
    let tenant = tenant.map(|namespace| ExtensionNamespace(namespace.to_owned()));
    let context = EntityContext {
        catalog: &catalog,
        registry: &registry,
        base: base.model(),
        tenant: tenant.as_ref(),
    };
    check_entity_model(&model.model(), &context)
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

/// Check `model` over the base fixture layer, as a registry layer.
fn findings(model: &serde_json::Value) -> Vec<(String, String)> {
    run(&Layer::from_json(model), &base_layer(), None)
}

fn detailed_findings(model: &serde_json::Value) -> Vec<Diagnostic> {
    let model = Layer::from_json(model);
    let base = base_layer();
    let catalog = catalog();
    let registry = registry();
    let context = EntityContext {
        catalog: &catalog,
        registry: &registry,
        base: base.model(),
        tenant: None,
    };
    check_entity_model(&model.model(), &context)
}

/// Check `model` over the base fixture layer, as tenant `acme`'s layer.
fn tenant_findings(model: &serde_json::Value) -> Vec<(String, String)> {
    run(&Layer::from_json(model), &base_layer(), Some("acme"))
}

fn one(code: &str, pointer: &str) -> Vec<(String, String)> {
    vec![(code.to_owned(), pointer.to_owned())]
}

#[test]
fn the_fixture_layers_are_valid() {
    let catalog = catalog();
    assert_eq!(
        norn_checker::check_property_type_defs(&catalog, &registry()),
        Vec::new()
    );
    let base = base_layer();
    assert_eq!(run(&base, &Layer::default(), None), Vec::new());
    assert_eq!(run(&tenant_layer(), &base, Some("acme")), Vec::new());
    assert_eq!(run(&tenant_link_layer(), &base, Some("acme")), Vec::new());

    // One interface, two implementors.
    let implementors: Vec<&str> = base
        .object_types
        .iter()
        .filter(|def| {
            def.implements
                .iter()
                .any(|name| name.as_str() == "party.Party")
        })
        .map(|def| def.name.as_str())
        .collect();
    assert_eq!(implementors, ["crm.Customer", "hr.Employee"]);
}

#[test]
fn a_tenant_layer_is_checked_against_its_base_and_namespace() {
    let base = base_layer();
    // The same tenant definitions are foreign to another tenant.
    let findings = run(&tenant_layer(), &base, Some("globex"));
    let codes_found: BTreeSet<&str> = findings.iter().map(|(code, _)| code.as_str()).collect();
    assert_eq!(
        codes_found,
        BTreeSet::from([
            codes::TENANT_TYPE_OUTSIDE_NAMESPACE,
            codes::UNNAMESPACED_MEMBER,
            codes::EXTENSION_NAMESPACE_MISMATCH,
        ])
    );
    // Without the base layer, the supertype and the extension target are
    // unknown, so `tier` is a new, unprefixed member rather than an override.
    let codes_found: BTreeSet<String> = run(&tenant_layer(), &Layer::default(), Some("acme"))
        .into_iter()
        .map(|(code, _)| code)
        .collect();
    assert_eq!(
        codes_found,
        BTreeSet::from([
            codes::UNKNOWN_SUPERTYPE.to_owned(),
            codes::UNKNOWN_EXTENSION_TARGET.to_owned(),
            codes::UNNAMESPACED_MEMBER.to_owned(),
        ])
    );
}

#[test]
fn every_code_has_a_negative_case() {
    let text = serde_json::json!({ "kind": "text" });
    let party_members = serde_json::json!([
        { "id": "display_name", "type": text },
        { "id": "email", "type": text }
    ]);
    let tier = |values: &[&str]| {
        serde_json::json!({
            "kind": "enum",
            "values": values.iter().map(|value| serde_json::json!({ "value": value })).collect::<Vec<_>>()
        })
    };
    let one_of_many = serde_json::json!({ "to": "one", "from": "many" });
    let required_one = serde_json::json!({ "to": "one", "from": "many", "required": true });
    let order_number = serde_json::json!({ "id": "order_number", "type": text });
    let registry_cases = [
        (
            interface_codes::INVALID_INHERITANCE,
            "/interfaces/0/extends",
            serde_json::json!({"interfaces":[{"name":"test.Child","extends":["test.Absent"]}]}),
        ),
        (
            codes::LINK_PROPERTY_MARKINGS_UNSUPPORTED,
            "/objectTypes/0/links/0/properties/0/markings",
            serde_json::json!({"objectTypes":[{"name":"test.Thing","links":[{"id":"owner","target":"crm.Customer","cardinality":one_of_many,"properties":[{"id":"email","type":{"kind":"text"},"markings":["pii.contact"]}]}]}]}),
        ),
        (
            codes::SHADOWS_BASE,
            "/objectTypes/0/name",
            serde_json::json!({ "objectTypes": [{ "name": "crm.Customer" }] }),
        ),
        (
            codes::INVALID_STATUS_DATE,
            "/objectTypes/0/status/since",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "status": { "state": "deprecated", "since": "2026-02-30" } }] }),
        ),
        (
            codes::UNKNOWN_REPLACEMENT,
            "/objectTypes/0/status/replacedBy",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "status": { "state": "deprecated", "since": "2026-09-30", "replacedBy": "test.Missing" } }] }),
        ),
        (
            codes::UNKNOWN_SUPERTYPE,
            "/objectTypes/0/extends",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "extends": "test.Missing" }] }),
        ),
        (
            codes::INHERITANCE_CYCLE,
            "/objectTypes/0/extends",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "extends": "test.Thing" }] }),
        ),
        (
            codes::OVERRIDE_TYPE_MISMATCH,
            "/objectTypes/0/properties/0/type",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
                "properties": [{ "id": "tier", "type": text }] }] }),
        ),
        (
            codes::OVERRIDE_WIDENS_NULLABILITY,
            "/objectTypes/0/properties/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
                "properties": [{ "id": "display_name", "type": text, "constraints": { "nullable": true } }] }] }),
        ),
        (
            codes::OVERRIDE_COMPUTED,
            "/objectTypes/0/properties/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
                "properties": [{ "id": "tier", "type": tier(&["gold"]), "computed": "'gold'" }] }] }),
        ),
        (
            codes::UNKNOWN_INTERFACE,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "implements": ["test.Missing"] }] }),
        ),
        (
            codes::INVALID_ACCOUNTING_LEG_DECLARATION,
            "/objectTypes/0/accountingLeg",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "accountingLeg": { "journalIdProperty": "leg", "legProperty": "leg" } }] }),
        ),
        (
            codes::INVALID_OWNED_COLLECTION,
            "/objectTypes/0/ownedCollections/0/maxRows",
            serde_json::json!({ "objectTypes": [
                { "name": "test.Parent",
                  "links": [{ "id": "lines", "target": "test.Child",
                    "cardinality": { "to": "many", "from": "one" } }],
                  "ownedCollections": [{ "id": "lines", "childType": "test.Child",
                    "rowIdProperty": "row_id", "orderingKey": "idx",
                    "cascadeOnParentDelete": true, "maxRows": 0 }] },
                { "name": "test.Child", "properties": [
                    { "id": "row_id", "type": text },
                    { "id": "idx", "type": { "kind": "integer" } }
                ] }
            ] }),
        ),
        (
            codes::DUPLICATE_INTERFACE,
            "/objectTypes/0/implements/1",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "implements": ["party.Party", "party.Party"], "properties": party_members }] }),
        ),
        (
            codes::MISSING_INTERFACE_PROPERTY,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "implements": ["party.Party"],
                "properties": [{ "id": "display_name", "type": text }] }] }),
        ),
        (
            codes::INTERFACE_PROPERTY_TYPE_MISMATCH,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "implements": ["party.Party"],
                "properties": [
                    { "id": "display_name", "type": { "kind": "integer" } },
                    { "id": "email", "type": text }
                ] }] }),
        ),
        (
            codes::INTERFACE_PROPERTY_WIDENS_NULLABILITY,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "implements": ["party.Party"],
                "properties": [
                    { "id": "display_name", "type": text, "constraints": { "nullable": true } },
                    { "id": "email", "type": text }
                ] }] }),
        ),
        (
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0",
            serde_json::json!({
                "interfaces": [{ "name": "test.Numbered",
                    "properties": [{ "id": "display_name", "type": { "kind": "integer" } }] }],
                "objectTypes": [{ "name": "test.Thing", "implements": ["party.Party", "test.Numbered"],
                    "properties": party_members }]
            }),
        ),
        (
            codes::RESERVED_MEMBER_SEPARATOR,
            "/objectTypes/0/properties/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "properties": [{ "id": "acme__tier", "type": text }] }] }),
        ),
        (
            codes::INVALID_EXTENSION_NAMESPACE,
            "/extensions/0/namespace",
            serde_json::json!({ "extensions": [{ "namespace": "acme_co", "target": "crm.Customer" }] }),
        ),
        (
            codes::UNKNOWN_EXTENSION_TARGET,
            "/extensions/0/target",
            serde_json::json!({ "extensions": [{ "namespace": "acme", "target": "crm.Missing" }] }),
        ),
        (
            codes::EXTENSION_SHADOWS_MEMBER,
            "/extensions/1/properties/0/id",
            serde_json::json!({ "extensions": [
                { "namespace": "acme", "target": "crm.Customer",
                    "properties": [{ "id": "acme__tier", "type": text }] },
                { "namespace": "acme", "target": "crm.KeyAccount",
                    "properties": [{ "id": "acme__tier", "type": text }] }
            ] }),
        ),
        (
            codes::EXTENSION_OF_LOCAL_TYPE,
            "/extensions/0/target",
            serde_json::json!({
                "objectTypes": [{ "name": "test.Thing" }],
                "extensions": [{ "namespace": "acme", "target": "test.Thing" }]
            }),
        ),
        (
            // crm.Customer caps display_name at maxLength 120.
            codes::UNSATISFIABLE_CONSTRAINTS,
            "/objectTypes/0/properties/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
                "properties": [{ "id": "display_name", "type": text, "constraints": { "minLength": 150 } }] }] }),
        ),
        (
            interface_codes::INVALID_NAME,
            "/interfaces/0/name",
            serde_json::json!({ "interfaces": [{ "name": "party.party" }] }),
        ),
        (
            interface_codes::DUPLICATE_NAME,
            "/interfaces/1/name",
            serde_json::json!({ "interfaces": [{ "name": "test.Named" }, { "name": "test.Named" }] }),
        ),
        (
            interface_codes::SHADOWS_BASE,
            "/interfaces/0/name",
            serde_json::json!({ "interfaces": [{ "name": "party.Party" }] }),
        ),
        (
            interface_codes::INVALID_PROPERTY_ID,
            "/interfaces/0/properties/0/id",
            serde_json::json!({ "interfaces": [{ "name": "test.Named",
                "properties": [{ "id": "legal__name", "type": text }] }] }),
        ),
        (
            interface_codes::DUPLICATE_PROPERTY_ID,
            "/interfaces/0/properties/1/id",
            serde_json::json!({ "interfaces": [{ "name": "test.Named",
                "properties": [{ "id": "name", "type": text }, { "id": "name", "type": text }] }] }),
        ),
        (
            interface_codes::INVALID_STATUS_DATE,
            "/interfaces/0/status/since",
            serde_json::json!({ "interfaces": [{ "name": "test.Named",
                "status": { "state": "deprecated", "since": "2026-13-01" } }] }),
        ),
        (
            interface_codes::UNKNOWN_REPLACEMENT,
            "/interfaces/0/status/replacedBy",
            serde_json::json!({ "interfaces": [{ "name": "test.Named",
                "status": { "state": "deprecated", "since": "2026-09-30", "replacedBy": "test.Named" } }] }),
        ),
        // T1-ONT-1d (#2514): links and invariants.
        (
            codes::INVALID_LINK_ID,
            "/objectTypes/0/links/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "links": [{ "id": "Owner", "target": "crm.Customer", "cardinality": one_of_many }] }] }),
        ),
        (
            codes::DUPLICATE_LINK_ID,
            "/objectTypes/0/links/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "properties": [{ "id": "owner", "type": text }],
                "links": [{ "id": "owner", "target": "crm.Customer", "cardinality": one_of_many }] }] }),
        ),
        (
            codes::UNKNOWN_LINK_TARGET,
            "/objectTypes/0/links/0/target",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "links": [{ "id": "owner", "target": "test.Missing", "cardinality": one_of_many }] }] }),
        ),
        (
            codes::MEMBER_ID_CONFLICT,
            "/objectTypes/1/links/0/inverse",
            serde_json::json!({ "objectTypes": [
                { "name": "test.Thing", "properties": [{ "id": "owners", "type": text }] },
                { "name": "test.Owner", "links": [{ "id": "thing", "target": "test.Thing",
                    "cardinality": one_of_many, "inverse": "owners" }] }
            ] }),
        ),
        (
            codes::FOREIGN_INVERSE,
            "/objectTypes/0/links/0/inverse",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "links": [{ "id": "customer", "target": "crm.Customer", "cardinality": one_of_many,
                    "inverse": "things" }] }] }),
        ),
        (
            // hr.Employee is a party.Party, but not a crm.Customer.
            codes::LINK_OVERRIDE_TARGET_MISMATCH,
            "/objectTypes/0/links/0/target",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
                "links": [{ "id": "customer", "target": "hr.Employee", "cardinality": required_one }] }] }),
        ),
        (
            codes::LINK_OVERRIDE_CHANGES_ACYCLIC,
            "/objectTypes/1/links/0/acyclic",
            serde_json::json!({ "objectTypes": [
                {"name":"test.Node", "links":[{"id":"parent", "target":"test.Node", "cardinality":{"to":"one","from":"many"}, "acyclic":true}]},
                {"name":"test.SubNode", "extends":"test.Node", "links":[{"id":"parent", "target":"test.Node", "cardinality":{"to":"one","from":"many"}, "acyclic":false}]}
            ]}),
        ),
        (
            codes::LINK_OVERRIDE_WIDENS_CARDINALITY,
            "/objectTypes/0/links/0/cardinality",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
                "links": [{ "id": "customer", "target": "crm.Customer",
                    "cardinality": { "to": "one", "from": "many" } }] }] }),
        ),
        (
            codes::LINK_OVERRIDE_REDECLARES,
            "/objectTypes/0/links/0/inverse",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
                "links": [{ "id": "customer", "target": "crm.Customer", "cardinality": required_one,
                    "inverse": "orders" }] }] }),
        ),
        (
            codes::MISSING_INTERFACE_LINK,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Order", "implements": ["commerce.Order"],
                "properties": [order_number] }] }),
        ),
        (
            // meta.Table does not implement party.Party.
            codes::INTERFACE_LINK_TARGET_MISMATCH,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Order", "implements": ["commerce.Order"],
                "properties": [order_number],
                "links": [{ "id": "customer", "target": "meta.Table", "cardinality": required_one }] }] }),
        ),
        (
            codes::INTERFACE_LINK_WIDENS_CARDINALITY,
            "/objectTypes/0/implements/0",
            serde_json::json!({ "objectTypes": [{ "name": "test.Order", "implements": ["commerce.Order"],
                "properties": [order_number],
                "links": [{ "id": "customer", "target": "crm.Customer", "cardinality": one_of_many }] }] }),
        ),
        (
            codes::INVALID_INVARIANT_ID,
            "/objectTypes/0/invariants/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "invariants": [{ "id": "Positive", "expression": "true", "message": "m" }] }] }),
        ),
        (
            // commerce.SalesOrder declares non_negative_total.
            codes::DUPLICATE_INVARIANT_ID,
            "/objectTypes/0/invariants/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
                "invariants": [{ "id": "non_negative_total", "expression": "self.total >= 1", "message": "m" }] }] }),
        ),
        (
            codes::EMPTY_INVARIANT_EXPRESSION,
            "/objectTypes/0/invariants/0/expression",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "invariants": [{ "id": "positive", "expression": " ", "message": "m" }] }] }),
        ),
        (
            codes::INVALID_INVARIANT_EXPRESSION,
            "/objectTypes/0/invariants/0/expression",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "invariants": [{ "id": "positive", "expression": "self.total >=", "message": "m" }] }] }),
        ),
        (
            codes::EMPTY_INVARIANT_MESSAGE,
            "/objectTypes/0/invariants/0/message",
            serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
                "invariants": [{ "id": "positive", "expression": "true", "message": "" }] }] }),
        ),
        (
            interface_codes::INVALID_LINK_ID,
            "/interfaces/0/links/0/id",
            serde_json::json!({ "interfaces": [{ "name": "test.Owned",
                "links": [{ "id": "acme__owner", "target": "party.Party", "cardinality": one_of_many }] }] }),
        ),
        (
            interface_codes::DUPLICATE_LINK_ID,
            "/interfaces/0/links/0/id",
            serde_json::json!({ "interfaces": [{ "name": "test.Owned",
                "properties": [{ "id": "owner", "type": text }],
                "links": [{ "id": "owner", "target": "party.Party", "cardinality": one_of_many }] }] }),
        ),
        (
            interface_codes::UNKNOWN_LINK_TARGET,
            "/interfaces/0/links/0/target",
            serde_json::json!({ "interfaces": [{ "name": "test.Owned",
                "links": [{ "id": "owner", "target": "test.Missing", "cardinality": one_of_many }] }] }),
        ),
    ];
    let tenant_cases = [
        (
            codes::UNNAMESPACED_MEMBER,
            "/objectTypes/0/properties/0/id",
            serde_json::json!({ "objectTypes": [{ "name": "acme.Thing", "extends": "crm.Customer",
                "properties": [{ "id": "credit", "type": text }] }] }),
        ),
        (
            codes::TENANT_TYPE_OUTSIDE_NAMESPACE,
            "/objectTypes/0/name",
            serde_json::json!({ "objectTypes": [{ "name": "crm.Special", "extends": "crm.Customer" }] }),
        ),
        (
            codes::TENANT_TYPE_NOT_SUBTYPE,
            "/objectTypes/0/name",
            serde_json::json!({ "objectTypes": [{ "name": "acme.Thing" }] }),
        ),
        (
            codes::EXTENSION_NAMESPACE_MISMATCH,
            "/extensions/0/namespace",
            serde_json::json!({ "extensions": [{ "namespace": "globex", "target": "crm.Customer" }] }),
        ),
        (
            interface_codes::TENANT_INTERFACE,
            "/interfaces/0/name",
            serde_json::json!({ "interfaces": [{ "name": "acme.Loyal" }] }),
        ),
    ];

    let covered: BTreeSet<&str> = registry_cases
        .iter()
        .chain(tenant_cases.iter())
        .map(|(code, _, _)| *code)
        .collect();
    let expected: BTreeSet<&str> = norn_checker::codes::REGISTRY
        .iter()
        .map(|entry| entry.code)
        .filter(|code| code.starts_with("interface."))
        .chain(MODEL_CODES)
        .collect();
    assert_eq!(
        covered, expected,
        "negative cases cover exactly the model codes"
    );

    for (code, pointer, value) in registry_cases {
        assert_eq!(findings(&value), one(code, pointer), "{value}");
    }
    for (code, pointer, value) in tenant_cases {
        assert_eq!(tenant_findings(&value), one(code, pointer), "{value}");
    }
}

#[test]
fn inherited_owned_collection_ids_cannot_be_redeclared_and_effective_views_are_nearest_first() {
    let text = serde_json::json!({ "kind": "text" });
    let model_json = serde_json::json!({ "objectTypes": [
        {
            "name": "test.Parent",
            "links": [{ "id": "lines", "target": "test.Line",
                "cardinality": { "to": "many", "from": "one" } }],
            "ownedCollections": [{ "id": "lines", "childType": "test.Line",
                "rowIdProperty": "row_id", "orderingKey": "idx",
                "cascadeOnParentDelete": true, "maxRows": 4 }]
        },
        {
            "name": "test.SpecialParent", "extends": "test.Parent",
            "ownedCollections": [{ "id": "lines", "childType": "test.Line",
                "rowIdProperty": "row_id", "orderingKey": "idx",
                "cascadeOnParentDelete": true, "maxRows": 2 }]
        },
        { "name": "test.Line", "properties": [
            { "id": "row_id", "type": text },
            { "id": "idx", "type": { "kind": "integer" } }
        ] }
    ] });
    let findings = findings(&model_json);
    assert!(findings.iter().any(|(code, pointer)| {
        code == codes::INVALID_OWNED_COLLECTION && pointer == "/objectTypes/1/ownedCollections/0/id"
    }));

    let layer = Layer::from_json(&model_json);
    let empty_base = Layer::default();
    let base_model = empty_base.model();
    let model = layer.model();
    let catalog = Vec::new();
    let registry = BTreeSet::new();
    let context = EntityContext {
        catalog: &catalog,
        registry: &registry,
        base: base_model,
        tenant: None,
    };
    let effective = effective_object_types(&model, &context);
    let collections = &effective[&1].0.owned_collections;
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].max_rows.0, 2);
}

#[test]
fn inherited_owned_collection_link_keeps_its_exact_child_type() {
    let text = serde_json::json!({ "kind": "text" });
    let findings = findings(&serde_json::json!({ "objectTypes": [
        {
            "name": "test.Parent",
            "links": [{ "id": "lines", "target": "test.Line",
                "cardinality": { "to": "many", "from": "one" } }],
            "ownedCollections": [{ "id": "lines", "childType": "test.Line",
                "rowIdProperty": "row_id", "orderingKey": "idx",
                "cascadeOnParentDelete": true, "maxRows": 4 }]
        },
        {
            "name": "test.SpecialParent", "extends": "test.Parent",
            "links": [{ "id": "lines", "target": "test.SpecialLine",
                "cardinality": { "to": "many", "from": "one" } }]
        },
        { "name": "test.Line", "properties": [
            { "id": "row_id", "type": text },
            { "id": "idx", "type": { "kind": "integer" } }
        ] },
        { "name": "test.SpecialLine", "extends": "test.Line" }
    ] }));
    assert!(findings.iter().any(|(code, pointer)| {
        code == codes::INVALID_OWNED_COLLECTION && pointer == "/objectTypes/1/links/0/target"
    }));
}

#[test]
fn accounting_leg_declarations_reject_unknown_wrong_nullable_and_remapped_properties() {
    let invalid = |model: serde_json::Value| {
        let defs: Vec<ObjectTypeDef> = serde_json::from_value(model["objectTypes"].clone())
            .expect("object type definitions decode");
        check_object_type_defs(&defs, &[], &BTreeSet::new())
            .into_iter()
            .any(|diagnostic| diagnostic.code.as_str() == codes::INVALID_ACCOUNTING_LEG_DECLARATION)
    };

    assert!(
        invalid(serde_json::json!({ "objectTypes": [{
        "name": "ledger.Entry",
        "accountingLeg": { "journalIdProperty": "missing_id", "legProperty": "missing_leg" }
    }] })),
        "unknown properties must be rejected"
    );

    assert!(
        invalid(serde_json::json!({ "objectTypes": [{
        "name": "ledger.Entry",
        "properties": [
            { "id": "journal_id", "type": { "kind": "integer" } },
            { "id": "leg", "type": { "kind": "text" } }
        ],
        "accountingLeg": { "journalIdProperty": "journal_id", "legProperty": "leg" }
    }] })),
        "wrong property kinds must be rejected"
    );

    assert!(
        invalid(serde_json::json!({ "objectTypes": [{
        "name": "ledger.Entry",
        "properties": [
            { "id": "journal_id", "type": { "kind": "text" }, "constraints": { "nullable": true } },
            { "id": "leg", "type": { "kind": "json-ext" }, "constraints": { "nullable": true } }
        ],
        "accountingLeg": { "journalIdProperty": "journal_id", "legProperty": "leg" }
    }] })),
        "nullable properties must be rejected"
    );

    assert!(
        invalid(serde_json::json!({ "objectTypes": [
        {
            "name": "ledger.Entry",
            "properties": [
                { "id": "journal_id", "type": { "kind": "text" } },
                { "id": "leg", "type": { "kind": "json-ext" } }
            ],
            "accountingLeg": { "journalIdProperty": "journal_id", "legProperty": "leg" }
        },
        {
            "name": "ledger.ChildEntry",
            "extends": "ledger.Entry",
            "properties": [
                { "id": "other_journal_id", "type": { "kind": "text" } },
                { "id": "other_leg", "type": { "kind": "json-ext" } }
            ],
            "accountingLeg": { "journalIdProperty": "other_journal_id", "legProperty": "other_leg" }
        }
    ] })),
        "subtypes must preserve the inherited property mapping"
    );
}

#[test]
fn inheritance_cycles_are_reported_on_every_type_on_the_cycle() {
    let findings = findings(&serde_json::json!({ "objectTypes": [
        { "name": "test.A", "extends": "test.B" },
        { "name": "test.B", "extends": "test.A" },
        { "name": "test.C", "extends": "test.A" }
    ] }));
    assert_eq!(
        findings,
        vec![
            (
                codes::INHERITANCE_CYCLE.to_owned(),
                "/objectTypes/0/extends".to_owned()
            ),
            (
                codes::INHERITANCE_CYCLE.to_owned(),
                "/objectTypes/1/extends".to_owned()
            ),
        ]
    );
}

#[test]
fn subtypes_inherit_keys_and_may_add_keys_over_inherited_properties() {
    let subtype = |identity: serde_json::Value| {
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.KeyAccount",
            "identity": identity }] }),
        )
    };
    // A new key over an inherited property.
    assert_eq!(
        subtype(
            serde_json::json!([{ "name": "by_manager", "properties": ["account_manager", "customer_number"] }])
        ),
        Vec::new()
    );
    assert_eq!(
        subtype(serde_json::json!([{ "name": "primary", "properties": ["account_manager"] }])),
        one(codes::DUPLICATE_KEY_NAME, "/objectTypes/0/identity/0/name")
    );
    assert_eq!(
        subtype(serde_json::json!([{ "name": "by_number", "properties": ["customer_number"] }])),
        one(codes::REDUNDANT_KEY, "/objectTypes/0/identity/0/properties")
    );
}

#[test]
fn inherited_and_interface_constraints_apply_to_defaults() {
    // party.Party requires `minLength: 1` on display_name.
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Thing", "implements": ["party.Party"],
            "properties": [
                { "id": "display_name", "type": { "kind": "text" }, "default": "" },
                { "id": "email", "type": { "kind": "text" } }
            ] }] })
        ),
        one(
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/objectTypes/0/properties/0/default"
        )
    );
    // crm.Customer caps display_name at 120 characters; a subtype's default
    // is held to it.
    let long = "x".repeat(121);
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
            "properties": [{ "id": "display_name", "type": { "kind": "text" }, "default": long }] }] })
        ),
        one(
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/objectTypes/0/properties/0/default"
        )
    );
}

#[test]
fn overrides_narrow_through_enums_formats_and_ref_chains() {
    let base = serde_json::json!({
        "name": "test.Base",
        "properties": [
            { "id": "code", "type": { "kind": "ref", "name": "commerce.sku_code" } },
            { "id": "contact", "type": { "kind": "text" } },
            { "id": "amount", "type": { "kind": "decimal", "scale": 2 } }
        ]
    });
    let with_override = |property: serde_json::Value| {
        findings(&serde_json::json!({ "objectTypes": [
            base.clone(),
            { "name": "test.Sub", "extends": "test.Base", "properties": [property] }
        ] }))
    };
    for narrowing in [
        serde_json::json!({ "id": "code", "type": { "kind": "ref", "name": "commerce.sku_code" },
            "constraints": { "maxLength": 12 } }),
        serde_json::json!({ "id": "contact", "type": { "kind": "text", "format": "email" } }),
    ] {
        assert_eq!(with_override(narrowing.clone()), Vec::new(), "{narrowing}");
    }
    for widening in [
        // Dropping the shared type drops its constraints.
        serde_json::json!({ "id": "code", "type": { "kind": "text" } }),
        serde_json::json!({ "id": "amount", "type": { "kind": "decimal", "scale": 3 } }),
        serde_json::json!({ "id": "contact", "type": { "kind": "integer" } }),
    ] {
        assert_eq!(
            with_override(widening.clone()),
            one(
                codes::OVERRIDE_TYPE_MISMATCH,
                "/objectTypes/1/properties/0/type"
            ),
            "{widening}"
        );
    }
}

#[test]
fn interface_requirements_through_supertypes_conflict_like_direct_ones() {
    // crm.KeyAccount implements party.Party through crm.Customer: a second
    // interface requiring an incompatible display_name is the diamond case.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [{ "name": "test.Numbered",
                "properties": [{ "id": "display_name", "type": { "kind": "integer" } }] }],
            "objectTypes": [{ "name": "test.Sub", "extends": "crm.KeyAccount", "implements": ["test.Numbered"] }]
        })),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
    // Requirements that one property can meet do not conflict: enums with a
    // common value.
    let status = |values: &[&str]| {
        serde_json::json!({ "kind": "enum",
        "values": values.iter().map(|value| serde_json::json!({ "value": value })).collect::<Vec<_>>() })
    };
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [
                { "name": "test.A", "properties": [{ "id": "state", "type": status(&["open", "closed"]) }] },
                { "name": "test.B", "properties": [{ "id": "state", "type": status(&["closed", "void"]) }] }
            ],
            "objectTypes": [{ "name": "test.Thing", "implements": ["test.A", "test.B"],
                "properties": [{ "id": "state", "type": status(&["closed"]) }] }]
        })),
        Vec::new()
    );
}

#[test]
fn extension_members_reach_subtypes_and_never_shadow() {
    let tenant = tenant_layer();
    // A tenant subtype of crm.KeyAccount may key on a member the tenant's
    // extension adds to crm.Customer.
    let mut layer = Layer::from_json(&serde_json::json!({ "objectTypes": [{
        "name": "acme.Flagship", "extends": "crm.KeyAccount",
        "identity": [{ "name": "by_points", "properties": ["customer_number", "acme__loyalty_points"] }]
    }] }));
    layer.extensions = tenant.extensions;
    assert_eq!(run(&layer, &base_layer(), Some("acme")), Vec::new());

    // An extension installed in the base layer is a member the next layer's
    // extensions cannot redeclare, on the target or a subtype.
    let mut base = base_layer();
    base.extensions = tenant_layer().extensions;
    let model = Layer::from_json(&serde_json::json!({ "extensions": [{
        "namespace": "acme", "target": "crm.KeyAccount",
        "properties": [{ "id": "acme__loyalty_points", "type": { "kind": "integer" } }]
    }] }));
    assert_eq!(
        run(&model, &base, Some("acme")),
        one(
            codes::EXTENSION_SHADOWS_MEMBER,
            "/extensions/0/properties/0/id"
        )
    );
}

#[test]
fn check_object_type_defs_keeps_bare_array_spans() {
    let defs: Vec<ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        { "name": "test.Base", "properties": [{ "id": "code", "type": { "kind": "text" } }] },
        { "name": "test.Sub", "extends": "test.Base",
            "properties": [{ "id": "code", "type": { "kind": "integer" } }] },
        { "name": "test.Orphan", "extends": "test.Missing" }
    ]))
    .expect("decodes");
    let found: Vec<(String, String)> = check_object_type_defs(&defs, &catalog(), &registry())
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            (
                codes::OVERRIDE_TYPE_MISMATCH.to_owned(),
                "/1/properties/0/type".to_owned()
            ),
            (codes::UNKNOWN_SUPERTYPE.to_owned(), "/2/extends".to_owned()),
        ]
    );
}

/// A base type with `code`, and an interface requiring `code`, for the
/// inherited-member cases.
fn coded(base_code: &serde_json::Value, requirement: &serde_json::Value) -> Vec<(String, String)> {
    findings(&serde_json::json!({
        "interfaces": [{ "name": "test.Coded", "properties": [requirement] }],
        "objectTypes": [
            { "name": "test.Base", "properties": [base_code] },
            { "name": "test.Sub", "extends": "test.Base", "implements": ["test.Coded"] }
        ]
    }))
}

#[test]
fn an_inherited_default_meets_the_interfaces_a_subtype_adds() {
    let requirement = serde_json::json!({ "id": "code", "type": { "kind": "text" },
        "constraints": { "minLength": 3 } });
    assert_eq!(
        coded(
            &serde_json::json!({ "id": "code", "type": { "kind": "text" }, "default": "x" }),
            &requirement
        ),
        one(
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/objectTypes/1/implements/0"
        )
    );
    assert_eq!(
        coded(
            &serde_json::json!({ "id": "code", "type": { "kind": "text" }, "default": "xyz" }),
            &requirement
        ),
        Vec::new()
    );
    // Inherited bounds and the new requirement's jointly admit nothing.
    assert_eq!(
        coded(
            &serde_json::json!({ "id": "code", "type": { "kind": "text" }, "constraints": { "maxLength": 2 } }),
            &requirement
        ),
        one(
            codes::UNSATISFIABLE_CONSTRAINTS,
            "/objectTypes/1/implements/0"
        )
    );
}

#[test]
fn an_override_without_a_default_keeps_the_inherited_one() {
    // crm.Customer's tier defaults to "standard"; narrowing tier to "gold"
    // without a new default leaves an inadmissible default.
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
            "properties": [{ "id": "tier",
                "type": { "kind": "enum", "values": [{ "value": "gold" }] } }] }] })
        ),
        one(codes::DEFAULT_TYPE_MISMATCH, "/objectTypes/0/properties/0")
    );
    // Overridden bounds keep applying to the inherited default.
    assert_eq!(
        findings(&serde_json::json!({ "objectTypes": [
            { "name": "test.Base", "properties": [
                { "id": "count", "type": { "kind": "integer" }, "default": "5" }] },
            { "name": "test.Sub", "extends": "test.Base", "properties": [
                { "id": "count", "type": { "kind": "integer" }, "constraints": { "min": "10" } }] }
        ] })),
        one(
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/objectTypes/1/properties/0"
        )
    );
}

#[test]
fn requirements_for_one_id_are_checked_together() {
    // minLength 5 and maxLength 3: each alone is fine, together empty.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [
                { "name": "test.Long", "properties": [
                    { "id": "code", "type": { "kind": "text" }, "constraints": { "minLength": 5 } }] },
                { "name": "test.Short", "properties": [
                    { "id": "code", "type": { "kind": "text" }, "constraints": { "maxLength": 3 } }] }
            ],
            "objectTypes": [{ "name": "test.Thing", "implements": ["test.Long", "test.Short"],
                "properties": [{ "id": "code", "type": { "kind": "text" } }] }]
        })),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
    // Three enums that overlap pairwise but share no value.
    let status = |values: &[&str]| {
        serde_json::json!({ "kind": "enum",
        "values": values.iter().map(|value| serde_json::json!({ "value": value })).collect::<Vec<_>>() })
    };
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [
                { "name": "test.A", "properties": [{ "id": "state", "type": status(&["a", "b"]) }] },
                { "name": "test.B", "properties": [{ "id": "state", "type": status(&["b", "c"]) }] },
                { "name": "test.C", "properties": [{ "id": "state", "type": status(&["a", "c"]) }] }
            ],
            "objectTypes": [{ "name": "test.Thing", "implements": ["test.A", "test.B", "test.C"],
                "properties": [{ "id": "state", "type": status(&["a"]) }] }]
        })),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
    // Numeric bounds across requirements.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [
                { "name": "test.Big", "properties": [
                    { "id": "n", "type": { "kind": "integer" }, "constraints": { "min": "10" } }] },
                { "name": "test.Small", "properties": [
                    { "id": "n", "type": { "kind": "integer" }, "constraints": { "max": "9" } }] }
            ],
            "objectTypes": [{ "name": "test.Thing", "implements": ["test.Big", "test.Small"],
                "properties": [{ "id": "n", "type": { "kind": "integer" } }] }]
        })),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
}

// ----- T1-ONT-1d (#2514): links and invariants ---------------------------------

fn link(id: &str, target: &str, cardinality: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "id": id, "target": target, "cardinality": cardinality })
}

fn one_of_many() -> serde_json::Value {
    serde_json::json!({ "to": "one", "from": "many" })
}

#[test]
fn inverse_roles_are_members_of_the_target_and_its_subtypes() {
    // commerce.SalesOrder's `customer` link names the inverse role `orders`
    // on crm.Customer, which a subtype inherits.
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer",
            "properties": [{ "id": "orders", "type": { "kind": "integer" } }] }] })
        ),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/properties/0/id")
    );
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "crm.KeyAccount",
            "links": [link("billing_profile", "crm.BillingProfile", &one_of_many())] }] })
        ),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/links/0/id")
    );
    // A subtype may not turn an inherited link into a property, or the
    // reverse.
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
            "properties": [{ "id": "customer", "type": { "kind": "text" } }] }] })
        ),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/properties/0/id")
    );
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Sub", "extends": "commerce.SalesOrder",
            "links": [link("total", "meta.Table", &one_of_many())] }] })
        ),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/links/0/id")
    );
}

#[test]
fn inverse_roles_collide_once_and_on_the_later_declaration() {
    // A self-link whose inverse is its own id.
    assert_eq!(
        findings(&serde_json::json!({ "objectTypes": [{ "name": "test.Node",
            "links": [{ "id": "parent", "target": "test.Node", "cardinality": one_of_many(),
                "inverse": "parent" }] }] })),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/links/0/inverse")
    );
    // Two links naming one inverse role on one target.
    assert_eq!(
        findings(&serde_json::json!({ "objectTypes": [
            { "name": "test.Target" },
            { "name": "test.A", "links": [{ "id": "target", "target": "test.Target",
                "cardinality": one_of_many(), "inverse": "sources" }] },
            { "name": "test.B", "links": [{ "id": "target", "target": "test.Target",
                "cardinality": one_of_many(), "inverse": "sources" }] }
        ] })),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/2/links/0/inverse")
    );
    // An inverse role on an interface is a member of every implementor and
    // may not take a required id; reported once.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [{ "name": "test.Named",
                "properties": [{ "id": "name", "type": { "kind": "text" } }] }],
            "objectTypes": [
                { "name": "test.Person", "implements": ["test.Named"],
                    "properties": [{ "id": "name", "type": { "kind": "text" } }] },
                { "name": "test.Tag", "links": [{ "id": "named", "target": "test.Named",
                    "cardinality": { "to": "many", "from": "many" }, "inverse": "name" }] }
            ]
        })),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/1/links/0/inverse")
    );
    // A distinct inverse role on an interface reaches implementors without
    // conflict.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [{ "name": "test.Named" }],
            "objectTypes": [
                { "name": "test.Person", "implements": ["test.Named"] },
                { "name": "test.Tag", "links": [{ "id": "named", "target": "test.Named",
                    "cardinality": { "to": "many", "from": "many" }, "inverse": "tags" }] },
                { "name": "test.Employee", "extends": "test.Person",
                    "properties": [{ "id": "tags", "type": { "kind": "text" } }] }
            ]
        })),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/2/properties/0/id")
    );
}

#[test]
fn link_overrides_narrow_target_and_cardinality() {
    let sub = |links: serde_json::Value| {
        findings(&serde_json::json!({ "objectTypes": [{ "name": "test.Sub",
            "extends": "commerce.SalesOrder", "links": links }] }))
    };
    // A subtype of the target, and one-to-one: both narrow.
    assert_eq!(
        sub(serde_json::json!([link(
            "customer",
            "crm.KeyAccount",
            &serde_json::json!({ "to": "one", "from": "one", "required": true })
        )])),
        Vec::new()
    );
    // Optional where the inherited link is required.
    assert_eq!(
        sub(serde_json::json!([link(
            "customer",
            "crm.Customer",
            &one_of_many()
        )])),
        one(
            codes::LINK_OVERRIDE_WIDENS_CARDINALITY,
            "/objectTypes/0/links/0/cardinality"
        )
    );
    // An override inherits link properties.
    assert_eq!(
        findings(
            &serde_json::json!({ "objectTypes": [{ "name": "test.Lead", "extends": "projects.Project",
            "links": [{ "id": "members", "target": "hr.Employee",
                "cardinality": { "to": "one", "from": "many" },
                "properties": [{ "id": "role", "type": { "kind": "text" } }] }] }] })
        ),
        one(
            codes::LINK_OVERRIDE_REDECLARES,
            "/objectTypes/0/links/0/properties"
        )
    );
    // An interface target narrows to a type implementing it, never the
    // reverse.
    let base = serde_json::json!({ "name": "test.Base",
        "links": [link("party", "party.Party", &one_of_many())] });
    let with_override = |target: &str, target_back: &str| {
        findings(&serde_json::json!({ "objectTypes": [
            base.clone(),
            { "name": "test.Sub", "extends": "test.Base", "links": [link("party", target, &one_of_many())] },
            { "name": "test.Back", "links": [link("thing", target_back, &one_of_many())] },
            { "name": "test.BackSub", "extends": "test.Back", "links": [link("thing", "party.Party", &one_of_many())] }
        ] }))
    };
    assert_eq!(
        with_override("hr.Employee", "crm.Customer"),
        one(
            codes::LINK_OVERRIDE_TARGET_MISMATCH,
            "/objectTypes/3/links/0/target"
        )
    );
}

#[test]
fn interface_link_requirements_are_met_by_narrowing_links() {
    let order = serde_json::json!({ "id": "order_number", "type": { "kind": "text" } });
    // An inherited link meets the requirement a subtype's interface adds.
    assert_eq!(
        findings(&serde_json::json!({ "objectTypes": [{ "name": "test.Rush",
            "extends": "commerce.SalesOrder", "implements": ["commerce.Order"] }] })),
        Vec::new()
    );
    // An inverse role is not a link the type declares.
    assert_eq!(
        findings(&serde_json::json!({
            "interfaces": [{ "name": "test.Ordered",
                "links": [link("orders", "commerce.SalesOrder", &serde_json::json!({ "to": "many", "from": "many" }))] }],
            "objectTypes": [{ "name": "test.Sub", "extends": "crm.Customer", "implements": ["test.Ordered"] }]
        })),
        one(codes::MISSING_INTERFACE_LINK, "/objectTypes/0/implements/0")
    );
    // A property and a link required under one id, and link targets no one
    // type narrows, conflict; an interface target never does.
    let conflict = |first: serde_json::Value, second: serde_json::Value| {
        findings(&serde_json::json!({
            "interfaces": [
                { "name": "test.A", "properties": first["properties"].clone(), "links": first["links"].clone() },
                { "name": "test.B", "properties": second["properties"].clone(), "links": second["links"].clone() }
            ],
            "objectTypes": [{ "name": "test.Thing", "implements": ["test.A", "test.B"],
                "properties": [order.clone()],
                "links": [link("owner", "crm.KeyAccount", &one_of_many())] }]
        }))
    };
    let requires = |target: &str| serde_json::json!({ "properties": [], "links": [link("owner", target, &one_of_many())] });
    assert_eq!(
        conflict(
            serde_json::json!({ "properties": [{ "id": "owner", "type": { "kind": "text" } }], "links": [] }),
            requires("crm.Customer")
        ),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
    assert_eq!(
        conflict(requires("crm.Customer"), requires("hr.Employee")),
        one(
            codes::INTERFACE_REQUIREMENT_CONFLICT,
            "/objectTypes/0/implements/0"
        )
    );
    assert_eq!(
        conflict(requires("crm.Customer"), requires("party.Party")),
        Vec::new()
    );
}

#[test]
fn interface_invariants_are_boolean_typed_and_inherited_by_implementations() {
    let model = |expression: &str| {
        serde_json::json!({
            "interfaces": [{
                "name": "test.TotalBearing",
                "properties": [{ "id": "total", "type": { "kind": "integer" } }],
                "invariants": [{ "id": "total_rule", "expression": expression, "message": "valid total" }]
            }],
            "objectTypes": [
                { "name": "test.Base", "implements": ["test.TotalBearing"],
                    "properties": [{ "id": "total", "type": { "kind": "integer" } }] },
                { "name": "test.Sub", "extends": "test.Base" }
            ]
        })
    };
    assert_eq!(findings(&model("self.total >= 0")), Vec::new());

    let missing = findings(&model("self.missing >= 0"));
    assert!(missing.contains(&(
        codes::INVALID_INVARIANT_EXPRESSION.to_owned(),
        "/interfaces/0/invariants/0/expression".to_owned()
    )));

    let non_boolean = findings(&model("self.total"));
    assert!(non_boolean.contains(&(
        codes::INVALID_INVARIANT_EXPRESSION.to_owned(),
        "/interfaces/0/invariants/0/expression".to_owned()
    )));

    let collision = serde_json::json!({
        "id": "shared_rule",
        "expression": "true",
        "message": "always true"
    });
    let object_collision = findings(&serde_json::json!({
        "interfaces": [{
            "name": "test.TotalBearing",
            "invariants": [collision]
        }],
        "objectTypes": [{
            "name": "test.Total",
            "implements": ["test.TotalBearing"],
            "invariants": [collision]
        }]
    }));
    assert!(object_collision.contains(&(
        codes::DUPLICATE_INVARIANT_ID.to_owned(),
        "/objectTypes/0/invariants/0/id".to_owned()
    )));

    let interface_collision = findings(&serde_json::json!({
        "interfaces": [
            { "name": "test.First", "invariants": [collision] },
            { "name": "test.Second", "invariants": [collision] }
        ],
        "objectTypes": [{
            "name": "test.Total",
            "implements": ["test.First", "test.Second"]
        }]
    }));
    assert!(interface_collision.contains(&(
        codes::DUPLICATE_INVARIANT_ID.to_owned(),
        "/objectTypes/0/implements/1".to_owned()
    )));
}

#[test]
fn temporal_ordering_invariants_require_parsing_and_refuse_mixed_kinds() {
    let model = |expression: &str| {
        serde_json::json!({
            "objectTypes": [{
                "name": "test.Season",
                "properties": [
                    { "id": "season_start", "type": { "kind": "date" } },
                    { "id": "season_end", "type": { "kind": "date" } },
                    { "id": "occurred_at_utc", "type": { "kind": "timestamp", "timezone": "utc" } },
                    { "id": "occurred_at_offset", "type": { "kind": "timestamp", "timezone": "offset" } },
                    { "id": "label", "type": { "kind": "text" } }
                ],
                "invariants": [{
                    "id": "season_window",
                    "expression": expression,
                    "message": "season end must not be before season start"
                }]
            }]
        })
    };

    for expression in [
        "date(self.season_end) >= date(self.season_start)",
        "date(self.season_start) < date(self.season_end)",
        "date(self.season_start) <= date(self.season_end)",
        "date(self.season_start) > date(self.season_end)",
        "date(self.season_start) >= date(self.season_end)",
        "timestamp(self.occurred_at_utc) < timestamp(self.occurred_at_offset)",
        "timestamp(self.occurred_at_utc) <= timestamp(self.occurred_at_offset)",
        "timestamp(self.occurred_at_utc) > timestamp(self.occurred_at_offset)",
        "timestamp(self.occurred_at_utc) >= timestamp(self.occurred_at_offset)",
    ] {
        assert_eq!(findings(&model(expression)), Vec::new(), "{expression}");
    }

    for expression in [
        "date(self.season_start) < timestamp(self.occurred_at_utc)",
        "self.season_start < self.season_end",
        "self.occurred_at_utc < self.occurred_at_offset",
        "self.season_start < self.occurred_at_utc",
        "self.occurred_at_offset >= self.season_end",
        "self.season_start < self.label",
        "self.label >= self.occurred_at_utc",
    ] {
        let diagnostics = findings(&model(expression));
        assert!(
            diagnostics.contains(&(
                codes::INVALID_INVARIANT_EXPRESSION.to_owned(),
                "/objectTypes/0/invariants/0/expression".to_owned()
            )),
            "expected mixed, raw-wire, or text ordering to be rejected: {expression}; got {diagnostics:?}"
        );
    }
}

#[test]
fn object_invariants_cannot_read_forward_or_inverse_links() {
    let assert_link_invariant_rejected = |model: serde_json::Value, pointer: &str| {
        let diagnostics = detailed_findings(&model);
        assert!(
            diagnostics.iter().any(|finding| {
                finding.code == codes::INVALID_INVARIANT_EXPRESSION
                    && finding
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == pointer)
                    && finding
                        .message
                        .contains("cross-object constraints are not yet enforced; see epic #3121")
            }),
            "expected cross-object invariant refusal at {pointer}; got {diagnostics:?}"
        );
    };

    // Literal bracket access is a member read too, and a property-only
    // invariant on the same type remains valid.
    let direct = serde_json::json!({
        "objectTypes": [{
            "name": "test.Group",
            "properties": [{ "id": "total", "type": { "kind": "integer" } }],
            "links": [{ "id": "children", "target": "test.Group",
                "cardinality": { "to": "many", "from": "many" } }],
            "invariants": [
                { "id": "bounded_children", "expression": "len(self.children) <= 1", "message": "bounded" },
                { "id": "bracket_children", "expression": "len(self[\"children\"]) <= 1", "message": "bounded" },
                { "id": "positive_total", "expression": "self.total > 0", "message": "positive" }
            ]
        }]
    });
    assert_link_invariant_rejected(direct.clone(), "/objectTypes/0/invariants/0/expression");
    assert_link_invariant_rejected(direct.clone(), "/objectTypes/0/invariants/1/expression");
    assert!(
        !findings(&direct)
            .iter()
            .any(|(_, pointer)| { pointer == "/objectTypes/0/invariants/2/expression" })
    );

    let inverse = serde_json::json!({
        "objectTypes": [
            { "name": "test.Parent", "links": [{
                "id": "child", "target": "test.Child",
                "cardinality": { "to": "one", "from": "many" }, "inverse": "parents"
            }] },
            { "name": "test.Child", "invariants": [{
                "id": "bounded_parents", "expression": "len(self.parents) <= 1", "message": "bounded"
            }] }
        ]
    });
    assert_link_invariant_rejected(inverse, "/objectTypes/1/invariants/0/expression");

    let inherited = serde_json::json!({
        "objectTypes": [
            { "name": "test.Base", "links": [{
                "id": "children", "target": "test.Child",
                "cardinality": { "to": "many", "from": "many" }
            }] },
            { "name": "test.Sub", "extends": "test.Base", "invariants": [{
                "id": "bounded_children", "expression": "len(self.children) <= 1", "message": "bounded"
            }] },
            { "name": "test.Child" }
        ]
    });
    assert_link_invariant_rejected(inherited, "/objectTypes/1/invariants/0/expression");
}

#[test]
fn dynamic_member_access_remains_rejected_by_invariant_type_checking() {
    let diagnostics = detailed_findings(&serde_json::json!({
        "objectTypes": [{
            "name": "test.Group",
            "properties": [{ "id": "key", "type": { "kind": "text" } }],
            "links": [{ "id": "children", "target": "test.Group",
                "cardinality": { "to": "many", "from": "many" } }],
            "invariants": [{
                "id": "dynamic_member", "expression": "self[self.key] == true", "message": "dynamic"
            }]
        }]
    }));
    assert!(diagnostics.iter().any(|finding| {
        finding.code == codes::INVALID_INVARIANT_EXPRESSION
            && finding
                .span
                .as_ref()
                .is_some_and(|span| span.pointer == "/objectTypes/0/invariants/0/expression")
    }));
    assert!(diagnostics.iter().all(|finding| {
        !finding
            .message
            .contains("cross-object constraints are not yet enforced")
    }));
}

#[test]
fn link_properties_follow_the_property_rules() {
    let with_properties = |properties: serde_json::Value| {
        findings(&serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
            "links": [{ "id": "owner", "target": "crm.Customer", "cardinality": one_of_many(),
                "properties": properties }] }] }))
    };
    assert_eq!(
        with_properties(serde_json::json!([
            { "id": "since", "type": { "kind": "date" } },
            { "id": "since", "type": { "kind": "date" } }
        ])),
        one(
            codes::DUPLICATE_PROPERTY_ID,
            "/objectTypes/0/links/0/properties/1/id"
        )
    );
    assert_eq!(
        with_properties(serde_json::json!([{ "id": "acme__since", "type": { "kind": "date" } }])),
        one(
            codes::RESERVED_MEMBER_SEPARATOR,
            "/objectTypes/0/links/0/properties/0/id"
        )
    );
    assert_eq!(
        with_properties(
            serde_json::json!([{ "id": "weight", "type": { "kind": "integer" },
            "constraints": { "min": "1" }, "default": "0" }])
        ),
        one(
            codes::DEFAULT_VIOLATES_CONSTRAINT,
            "/objectTypes/0/links/0/properties/0/default"
        )
    );
}

#[test]
fn tenants_add_namespaced_links_and_only_narrow_base_links() {
    // A tenant subtype narrows an installed link.
    assert_eq!(
        tenant_findings(
            &serde_json::json!({ "objectTypes": [{ "name": "acme.RushOrder",
            "extends": "commerce.SalesOrder",
            "links": [link("customer", "crm.KeyAccount",
                &serde_json::json!({ "to": "one", "from": "many", "required": true }))] }] })
        ),
        Vec::new()
    );
    // ... and never widens it.
    assert_eq!(
        tenant_findings(
            &serde_json::json!({ "objectTypes": [{ "name": "acme.RushOrder",
            "extends": "commerce.SalesOrder",
            "links": [link("customer", "crm.Customer", &serde_json::json!({ "to": "many", "from": "many", "required": true }))] }] })
        ),
        one(
            codes::LINK_OVERRIDE_WIDENS_CARDINALITY,
            "/objectTypes/0/links/0/cardinality"
        )
    );
    // New links, inverse roles, and invariants of a tenant are namespaced.
    assert_eq!(
        tenant_findings(
            &serde_json::json!({ "objectTypes": [{ "name": "acme.RushOrder",
            "extends": "commerce.SalesOrder",
            "links": [{ "id": "acme__courier", "target": "hr.Employee", "cardinality": one_of_many(),
                "inverse": "rush_orders" }],
            "invariants": [{ "id": "rush_total", "expression": "self.total > 0", "message": "m" }] }] })
        ),
        vec![
            (
                codes::UNNAMESPACED_MEMBER.to_owned(),
                "/objectTypes/0/links/0/inverse".to_owned()
            ),
            (
                codes::UNNAMESPACED_MEMBER.to_owned(),
                "/objectTypes/0/invariants/0/id".to_owned()
            ),
        ]
    );
    assert_eq!(
        tenant_findings(&serde_json::json!({ "extensions": [{ "namespace": "acme",
            "target": "crm.Customer", "links": [link("referrer", "crm.Customer", &one_of_many())] }] })),
        one(codes::UNNAMESPACED_MEMBER, "/extensions/0/links/0/id")
    );
    // Base-layer links and invariants never use the separator.
    assert_eq!(
        findings(&serde_json::json!({ "objectTypes": [{ "name": "test.Thing",
            "invariants": [{ "id": "acme__rule", "expression": "true", "message": "m" }] }] })),
        one(
            codes::RESERVED_MEMBER_SEPARATOR,
            "/objectTypes/0/invariants/0/id"
        )
    );
    // An extension link may not take a member id its target already has.
    let mut layer = tenant_layer();
    layer.object_types.clear();
    layer.extensions.push(parse(
        &serde_json::json!({ "namespace": "acme", "target": "crm.Customer",
            "links": [link("acme__loyalty_points", "crm.Customer", &one_of_many())] })
        .to_string(),
    ));
    assert_eq!(
        run(&layer, &base_layer(), Some("acme")),
        one(codes::EXTENSION_SHADOWS_MEMBER, "/extensions/1/links/0/id")
    );
    // The fixture's namespaced inverse role reaches subtypes of the target.
    let mut layer = tenant_link_layer();
    layer.object_types.push(parse(
        &serde_json::json!({ "name": "acme.Flagship", "extends": "crm.KeyAccount",
            "properties": [{ "id": "acme__referrals", "type": { "kind": "integer" } }] })
        .to_string(),
    ));
    assert_eq!(
        run(&layer, &base_layer(), Some("acme")),
        one(codes::MEMBER_ID_CONFLICT, "/objectTypes/0/properties/0/id")
    );
}

#[test]
fn links_in_a_bare_array_resolve_among_the_array() {
    let defs: Vec<ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        { "name": "test.Owner" },
        { "name": "test.Pet", "links": [
            { "id": "owner", "target": "test.Owner", "cardinality": one_of_many(), "inverse": "pets" },
            { "id": "vet", "target": "test.Vet", "cardinality": one_of_many() }
        ] }
    ]))
    .expect("decodes");
    let found: Vec<(String, String)> = check_object_type_defs(&defs, &catalog(), &registry())
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![(
            codes::UNKNOWN_LINK_TARGET.to_owned(),
            "/1/links/1/target".to_owned()
        )]
    );
}

#[test]
fn subtype_only_acyclic_override_refuses_mixed_base_subtype_bypass() {
    for (base, sub, admits) in [
        (false, true, false),
        (true, true, true),
        (true, false, false),
    ] {
        let diagnostics = findings(&serde_json::json!({"objectTypes":[
            {"name":"test.Node", "links":[{"id":"parent", "target":"test.Node", "cardinality":one_of_many(), "acyclic":base}]},
            {"name":"test.TreeNode", "extends":"test.Node", "links":[{"id":"parent", "target":"test.Node", "cardinality":one_of_many(), "acyclic":sub}]}
        ]}));
        let constraint = diagnostics
            .iter()
            .filter(|(_, pointer)| pointer == "/objectTypes/1/links/0/acyclic")
            .count();
        assert_eq!(constraint, usize::from(!admits), "{diagnostics:?}");
        if !admits {
            assert_eq!(
                diagnostics,
                vec![(
                    "object_type.validation.link_override_changes_acyclic".to_owned(),
                    "/objectTypes/1/links/0/acyclic".to_owned(),
                )]
            );
        }
        if admits {
            assert!(diagnostics.is_empty());
        }
    }
}

#[test]
fn link_property_catalog_markings_are_refused_after_resolution() {
    let base = base_layer();
    let mut catalog = catalog();
    catalog.push(
        serde_json::from_value(serde_json::json!({
            "name":"test.private_text", "type":{"kind":"text"}, "markings":["pii.contact"]
        }))
        .expect("property type"),
    );
    let layer = Layer::from_json(&serde_json::json!({"objectTypes":[{
        "name":"test.Thing", "links":[{"id":"owner","target":"crm.Customer","cardinality":one_of_many(),
        "properties":[{"id":"email","type":{"kind":"ref","name":"test.private_text"}}]}]
    }]}));
    let diagnostics = check_entity_model(
        &layer.model(),
        &EntityContext {
            catalog: &catalog,
            registry: &registry(),
            base: base.model(),
            tenant: None,
        },
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == codes::LINK_PROPERTY_MARKINGS_UNSUPPORTED)
    );
}

#[test]
fn interface_diamonds_inherit_members_constraints_links_and_invariants_once() {
    let mut model = serde_json::json!({
        "interfaces": [
            {"name":"test.Root","properties":[{"id":"code","type":{"kind":"text"},"constraints":{"minLength":3}}],
             "invariants":[{"id":"valid_code","expression":"self.code != ''","message":"code is present"}]},
            {"name":"test.Left","extends":["test.Root"]},
            {"name":"test.Right","extends":["test.Root"]},
            {"name":"test.Diamond","extends":["test.Left","test.Right"],
             "links":[{"id":"parent","target":"test.Root","cardinality":{"to":"one","from":"many","required":true}}]}
        ],
        "objectTypes":[{"name":"test.Thing","implements":["test.Diamond"],
            "properties":[{"id":"code","type":{"kind":"text"}}],
            "links":[{"id":"parent","target":"test.Diamond","cardinality":{"to":"one","from":"many","required":true}}]}]
    });
    assert_eq!(findings(&model), vec![]);
    model["objectTypes"][0]["properties"][0]["default"] = serde_json::json!("x");
    assert!(
        findings(&model)
            .iter()
            .any(|(code, _)| code == codes::DEFAULT_VIOLATES_CONSTRAINT)
    );
    model["objectTypes"][0]["properties"] = serde_json::json!([]);
    assert!(
        findings(&model)
            .iter()
            .any(|(code, _)| code == codes::MISSING_INTERFACE_PROPERTY)
    );
}

#[test]
fn interface_parent_graphs_and_diamond_conflicts_are_checked_without_implementers() {
    let invalid = interface_codes::INVALID_INHERITANCE;
    for interfaces in [
        serde_json::json!([{"name":"test.Child","extends":["test.Absent"]}]),
        serde_json::json!([{"name":"test.Child","extends":["test.Child"]}]),
        serde_json::json!([{"name":"test.First","extends":["test.Second"]},{"name":"test.Second","extends":["test.First"]}]),
        serde_json::json!([{"name":"test.Root"},{"name":"test.Child","extends":["test.Root","test.Root"]}]),
        serde_json::json!([
            {"name":"test.Left","properties":[{"id":"code","type":{"kind":"text"},"constraints":{"minLength":5}}]},
            {"name":"test.Right","properties":[{"id":"code","type":{"kind":"text"},"constraints":{"maxLength":3}}]},
            {"name":"test.Child","extends":["test.Left","test.Right"]}]),
        serde_json::json!([
            {"name":"test.Root","properties":[{"id":"code","type":{"kind":"text"}}]},
            {"name":"test.Child","extends":["test.Root"],"properties":[{"id":"code","type":{"kind":"integer"}}]}]),
    ] {
        let model = serde_json::json!({"interfaces":interfaces});
        assert!(
            findings(&model).iter().any(|(code, _)| code == invalid),
            "{model}"
        );
    }
}

#[test]
fn interface_invariants_defer_parent_member_typing_until_artifact_set_admission() {
    let definition: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name":"test.Child","extends":["test.Parent"],
        "invariants":[{"id":"present","expression":"self.code != ''","message":"code must be present"}]
    })).expect("interface");
    assert!(norn_checker::check_interface_local(&definition).is_empty());
    let model = serde_json::json!({"interfaces":[
        {"name":"test.Parent","properties":[{"id":"code","type":{"kind":"text"}}]},
        definition
    ]});
    assert!(findings(&model).is_empty());
}

#[test]
fn inverse_roles_cannot_shadow_members_of_descendant_interfaces_without_implementers() {
    let model = serde_json::json!({"interfaces":[
        {"name":"test.Parent"},
        {"name":"test.Child","extends":["test.Parent"],"properties":[{"id":"code","type":{"kind":"text"}}]}
    ],"objectTypes":[{"name":"test.Source","links":[{"id":"owner","target":"test.Parent","inverse":"code","cardinality":{"to":"one","from":"many"}}]}]});
    assert!(
        findings(&model)
            .iter()
            .any(|(code, _)| code == codes::MEMBER_ID_CONFLICT)
    );
}
