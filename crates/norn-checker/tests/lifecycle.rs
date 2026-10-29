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

//! Declared lifecycles (T3-LIFE-1, #2672): the fixture lifecycles (two
//! orthogonal machines, and a subtype that narrows one) are valid; every
//! `lifecycle.validation.*` code has a negative fixture in
//! `fixtures/ir/entity-v2/lifecycles/invalid/` (or, for the rule-write hook,
//! a case here); tenants and subtypes only narrow.

use std::collections::BTreeSet;

use norn_checker::{
    Diagnostic, EntityContext, EntityModel, LifecycleWrite, Span, check_entity_model,
    check_lifecycle_writes, check_object_type_local, lifecycles::codes,
};
use norn_ir::{ExtensionNamespace, ObjectTypeDef, PropertyTypeName};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/ir/entity-v2");

fn read(path: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{path}"))
        .unwrap_or_else(|error| panic!("{path} should be readable: {error}"))
}

fn object_type(path: &str) -> ObjectTypeDef {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{path} decodes: {error}"))
}

fn purchase_order() -> ObjectTypeDef {
    object_type("object-types/purchase-order.object-type.json")
}

fn rush_purchase_order() -> ObjectTypeDef {
    object_type("object-types/rush-purchase-order.object-type.json")
}

/// Check `object_types` as a layer over the base `base`, for `tenant`.
fn run(
    object_types: &[ObjectTypeDef],
    base: &[ObjectTypeDef],
    tenant: Option<&str>,
) -> Vec<(String, String)> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    let tenant = tenant.map(|namespace| ExtensionNamespace(namespace.to_owned()));
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base: EntityModel {
            object_types: base,
            ..EntityModel::EMPTY
        },
        tenant: tenant.as_ref(),
    };
    let model = EntityModel {
        object_types,
        ..EntityModel::EMPTY
    };
    pairs(check_entity_model(&model, &context))
}

fn pairs(findings: Vec<Diagnostic>) -> Vec<(String, String)> {
    findings
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn model_types(value: &serde_json::Value) -> Vec<ObjectTypeDef> {
    serde_json::from_value(value["objectTypes"].clone()).expect("object types decode")
}

#[test]
fn lifecycle_fixtures_are_valid() {
    let (base, subtype) = (purchase_order(), rush_purchase_order());
    assert_eq!(run(&[base.clone(), subtype.clone()], &[], None), Vec::new());
    // The subtype over the installed base, and each type checked alone.
    assert_eq!(
        run(
            std::slice::from_ref(&subtype),
            std::slice::from_ref(&base),
            None
        ),
        Vec::new()
    );
    assert_eq!(check_object_type_local(&base), Vec::new());
    assert_eq!(check_object_type_local(&subtype), Vec::new());
}

#[test]
fn every_lifecycle_code_has_a_negative_fixture() {
    let manifest: serde_json::Value =
        serde_json::from_str(&read("lifecycles/invalid/manifest.json")).expect("manifest");
    let base = [purchase_order()];
    let mut covered = BTreeSet::from([codes::UNDECLARED_WRITE.to_owned()]);
    let fixtures = manifest["fixtures"].as_array().expect("fixture list");
    for fixture in fixtures {
        let file = fixture["file"].as_str().expect("file");
        let code = fixture["code"].as_str().expect("code");
        let at = fixture["error_at"].as_str().expect("error_at");
        let document: serde_json::Value =
            serde_json::from_str(&read(&format!("lifecycles/invalid/{file}"))).expect("JSON");
        assert_eq!(
            run(&model_types(&document), &base, fixture["tenant"].as_str()),
            vec![(code.to_owned(), at.to_owned())],
            "{file}"
        );
        covered.insert(code.to_owned());
    }
    let files = std::fs::read_dir(format!("{FIXTURES}/lifecycles/invalid"))
        .expect("fixture directory")
        .count();
    assert_eq!(
        files,
        fixtures.len() + 1,
        "every fixture is in the manifest"
    );
    let registered: BTreeSet<String> = norn_checker::codes::REGISTRY
        .iter()
        .map(|entry| entry.code)
        .filter(|code| code.starts_with("lifecycle."))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        covered, registered,
        "negative cases cover exactly the lifecycle codes"
    );
}

#[test]
fn immutable_lifecycle_states_must_be_declared_once() {
    let mut unknown = purchase_order();
    unknown.lifecycles[0].immutable = vec!["not_a_state".to_owned()];
    assert_eq!(
        pairs(check_object_type_local(&unknown)),
        vec![(
            codes::UNKNOWN_STATE.to_owned(),
            "/lifecycles/0/immutable/0".to_owned()
        )]
    );

    let mut duplicate = purchase_order();
    duplicate.lifecycles[0].immutable = vec!["submitted".to_owned(), "submitted".to_owned()];
    assert_eq!(
        pairs(check_object_type_local(&duplicate)),
        vec![(
            codes::DUPLICATE_STATE.to_owned(),
            "/lifecycles/0/immutable/1".to_owned()
        )]
    );
}

#[test]
fn a_tenant_subtype_may_narrow_a_base_lifecycle() {
    let tenant: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "acme.StandingOrder",
        "extends": "commerce.PurchaseOrder",
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["received", "cancelled"],
            "transitions": [
                { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.po_number != \"\""] },
                { "id": "approve", "from": ["submitted"], "to": "approved" },
                { "id": "reject", "from": ["submitted"], "to": "rejected" },
                { "id": "revise", "from": ["rejected"], "to": "draft" },
                { "id": "receive", "from": ["approved"], "to": "received" },
                { "id": "cancel", "from": ["draft"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("decodes");
    assert_eq!(
        run(&[tenant], &[purchase_order()], Some("acme")),
        Vec::new()
    );
}

#[test]
fn a_subtype_inherits_lifecycles_it_does_not_narrow() {
    // A subtype that neither overrides `status` nor redeclares its lifecycle
    // is not re-checked, and a grandchild narrows the narrowed machine.
    let child: ObjectTypeDef = serde_json::from_value(
        serde_json::json!({ "name": "test.Child", "extends": "commerce.RushPurchaseOrder" }),
    )
    .expect("decodes");
    let widening: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.Grandchild",
        "extends": "test.Child",
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["received", "cancelled"],
            "transitions": [
                { "id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.po_number != \"\""] },
                { "id": "approve", "from": ["submitted"], "to": "approved" },
                { "id": "receive", "from": ["approved"], "to": "received" },
                { "id": "cancel", "from": ["draft", "submitted"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("decodes");
    assert_eq!(
        run(
            &[child, widening],
            &[purchase_order(), rush_purchase_order()],
            None
        ),
        vec![(
            codes::WIDENS_TRANSITION.to_owned(),
            "/objectTypes/1/lifecycles/0/transitions/3/from/1".to_owned()
        )],
        "the grandchild widens its parent's inherited (narrowed) cancel"
    );
}

#[test]
fn a_lifecycle_on_an_inherited_property_is_not_a_local_finding() {
    let subtype: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.Sub",
        "extends": "commerce.PurchaseOrder",
        "lifecycles": [{ "property": "payment_status", "initial": ["unpaid"], "terminal": ["paid", "refunded"],
            "transitions": [{ "id": "pay", "from": ["unpaid"], "to": "paid" }] }]
    }))
    .expect("decodes");
    assert_eq!(check_object_type_local(&subtype), Vec::new());
    // Over the model: dropping `refund` strands `refunded`, which the
    // subtype would have to remove from the enum as well.
    assert_eq!(
        run(&[subtype], &[purchase_order()], None),
        vec![(
            codes::UNREACHABLE_STATE.to_owned(),
            "/objectTypes/0/lifecycles/0".to_owned()
        )]
    );
}

#[test]
fn lifecycle_writes_follow_declared_transitions() {
    let def = purchase_order();
    let status = &def.lifecycles[0];
    let at = |index: usize| Span::pointer(format!("/writes/{index}"));
    let writes = [
        LifecycleWrite {
            from: None,
            to: "draft",
            at: at(0),
        },
        LifecycleWrite {
            from: Some("draft"),
            to: "submitted",
            at: at(1),
        },
        LifecycleWrite {
            from: Some("approved"),
            to: "cancelled",
            at: at(2),
        },
        // Unchanged state: not a transition.
        LifecycleWrite {
            from: Some("approved"),
            to: "approved",
            at: at(3),
        },
        // Undeclared: creation in a non-initial state, a skipped step, and a
        // way out of a terminal state.
        LifecycleWrite {
            from: None,
            to: "approved",
            at: at(4),
        },
        LifecycleWrite {
            from: Some("draft"),
            to: "approved",
            at: at(5),
        },
        LifecycleWrite {
            from: Some("received"),
            to: "draft",
            at: at(6),
        },
    ];
    assert_eq!(
        pairs(check_lifecycle_writes(status, &writes)),
        [4, 5, 6]
            .into_iter()
            .map(|index| (
                codes::UNDECLARED_WRITE.to_owned(),
                format!("/writes/{index}")
            ))
            .collect::<Vec<_>>()
    );
}

/// A subtype's redeclared `submit` with `guards`, over the purchase order.
fn submit_guards(guards: &serde_json::Value) -> Vec<(String, String)> {
    let subtype: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.CheckedOrder",
        "extends": "commerce.PurchaseOrder",
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["received", "cancelled"],
            "transitions": [
                { "id": "submit", "from": ["draft"], "to": "submitted", "guards": guards },
                { "id": "approve", "from": ["submitted"], "to": "approved" },
                { "id": "receive", "from": ["approved"], "to": "received" },
                { "id": "reject", "from": ["submitted"], "to": "rejected" },
                { "id": "revise", "from": ["rejected"], "to": "draft" },
                { "id": "cancel", "from": ["draft"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("decodes");
    run(&[subtype], &[purchase_order()], None)
}

#[test]
fn a_subtype_lifecycle_guard_conjoins_and_is_never_dropped() {
    // Restating the inherited guard and appending a stricter one is accepted:
    // the transition needs both.
    assert_eq!(
        submit_guards(&serde_json::json!([
            "self.po_number != \"\"",
            "len(self.po_number) == 10"
        ])),
        Vec::new()
    );
    // Restating it alone is accepted.
    assert_eq!(
        submit_guards(&serde_json::json!(["self.po_number != \"\""])),
        Vec::new()
    );
    let drops = vec![(
        codes::DROPS_GUARD.to_owned(),
        "/objectTypes/0/lifecycles/0/transitions/0/id".to_owned(),
    )];
    // Omitting it, or replacing it with a weaker one, is rejected.
    assert_eq!(submit_guards(&serde_json::json!([])), drops);
    assert_eq!(submit_guards(&serde_json::json!(["true"])), drops);
}

#[test]
fn guards_are_parsed_even_when_the_lifecycle_property_is_invalid() {
    // T1-ONT-2: guards are expression sites, checked independently of the
    // lifecycle's other checks (which stop at an unknown property).
    let model = serde_json::json!({ "objectTypes": [{
        "name": "test.Thing",
        "lifecycles": [{
            "property": "missing",
            "initial": ["open"],
            "transitions": [{ "id": "close", "from": ["open"], "to": "closed",
                "guards": ["self.ready and", " "] }]
        }]
    }] });
    let mut found = run(&model_types(&model), &[], None);
    found.sort();
    assert_eq!(
        found,
        vec![
            (
                codes::EMPTY_GUARD.to_owned(),
                "/objectTypes/0/lifecycles/0/transitions/0/guards/1".to_owned()
            ),
            (
                codes::INVALID_GUARD.to_owned(),
                "/objectTypes/0/lifecycles/0/transitions/0/guards/0".to_owned()
            ),
            (
                codes::UNKNOWN_PROPERTY.to_owned(),
                "/objectTypes/0/lifecycles/0/property".to_owned()
            ),
        ]
    );
}

fn path_fixture(target: &str, via: &str) -> ObjectTypeDef {
    serde_json::from_value(serde_json::json!({
        "name": "test.Order",
        "properties": [{"id": "status", "type": {"kind": "enum", "values": [
            {"value": "draft"}, {"value": "paid"}, {"value": "shipped"}
        ]}}],
        "lifecycles": [{"property": "status", "initial": ["draft"],
            "terminal": ["shipped"],
            "transitions": [
                {"id": "pay", "from": ["draft"], "to": "paid"},
                {"id": "ship", "from": ["paid"], "to": "shipped"}
            ],
            "pathProperties": [{"target": target, "via": via}]
        }]
    }))
    .expect("path fixture decodes")
}

#[test]
fn lifecycle_path_property_proves_required_visit() {
    assert!(run(&[path_fixture("shipped", "paid")], &[], None).is_empty());
    assert!(run(&[path_fixture("shipped", "draft")], &[], None).is_empty());
    assert!(run(&[path_fixture("shipped", "shipped")], &[], None).is_empty());
}

#[test]
fn lifecycle_path_property_returns_shortest_deterministic_trace() {
    let mut order = path_fixture("shipped", "paid");
    order.lifecycles[0].transitions.push(
        serde_json::from_value(serde_json::json!({
            "id": "ship_unpaid", "from": ["draft"], "to": "shipped"
        }))
        .expect("transition decodes"),
    );
    let registry = BTreeSet::new();
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base: EntityModel::EMPTY,
        tenant: None,
    };
    let types = [order];
    let model = EntityModel {
        object_types: &types,
        ..EntityModel::EMPTY
    };
    let diagnostics = check_entity_model(&model, &context);
    let finding = diagnostics
        .iter()
        .find(|d| d.code.as_str() == codes::PATH_PROPERTY_VIOLATED)
        .expect("unpaid shipping found");
    assert_eq!(
        finding.message,
        "a declared path to \"shipped\" avoids \"paid\"; structural trace [transition, state]: [[null,\"draft\"],[\"ship_unpaid\",\"shipped\"]]"
    );
    assert_eq!(
        finding.span.as_ref().expect("span").pointer,
        "/objectTypes/0/lifecycles/0"
    );
}

#[test]
fn lifecycle_path_property_rejects_unknown_states() {
    let result = run(&[path_fixture("shipped", "missing")], &[], None);
    assert!(result.iter().any(|(code, _)| code == codes::UNKNOWN_STATE));
}

#[test]
fn lifecycle_path_property_inherited_when_child_redeclares_machine() {
    let parent = path_fixture("shipped", "paid");
    let mut child = parent.clone();
    child.name = norn_ir::ObjectTypeName("test.Child".to_owned());
    child.extends = Some(parent.name.clone());
    child.properties.clear();
    child.lifecycles[0].path_properties.clear();
    let types = [parent, child];
    assert!(run(&types, &[], None).is_empty());
}

#[test]
fn inherited_path_property_survives_redeclaration_and_target_narrowing() {
    let parent = path_fixture("shipped", "paid");
    let mut child = parent.clone();
    child.name = norn_ir::ObjectTypeName("test.Child".to_owned());
    child.extends = Some(parent.name.clone());
    child.lifecycles[0].path_properties.clear();
    let lookup = |name: &str| {
        if name == parent.name.as_str() {
            Some(&parent)
        } else if name == child.name.as_str() {
            Some(&child)
        } else {
            None
        }
    };
    let machines = norn_ir::effective_lifecycles(&lookup, &[], child.name.as_str());
    assert_eq!(
        machines[&child.lifecycles[0].property].path_properties,
        parent.lifecycles[0].path_properties
    );
    child.properties[0].property_type = serde_json::from_value(serde_json::json!({
        "kind": "enum", "values": [{"value": "draft"}, {"value": "paid"}]
    }))
    .expect("narrowed enum decodes");
    child.lifecycles[0].terminal = vec!["paid".to_owned()];
    child.lifecycles[0].transitions.truncate(1);
    assert!(run(&[parent, child], &[], None).is_empty());
}
