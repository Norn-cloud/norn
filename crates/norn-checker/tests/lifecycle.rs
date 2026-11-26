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
    check_interface_local, check_lifecycle_writes, check_object_type_local, lifecycles::codes,
};
use norn_ir::{ExtensionNamespace, InterfaceDef, ObjectTypeDef, PropertyTypeName};

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
    let mut invalid = purchase_order();
    invalid.lifecycles[0].transitions[0].guard_refusal_code =
        Some("not.registered.code".to_owned());
    assert!(
        run(&[invalid], &[], None)
            .iter()
            .any(|(code, _)| code == codes::INVALID_GUARD_DIAGNOSTIC)
    );
    covered.insert(codes::INVALID_GUARD_DIAGNOSTIC.to_owned());
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
        // `lifecycle.soundness.*` (LIFE-SOUND) codes are whole-graph findings with their own
        // planted cases (`lifecycle_soundness` tests, the bounded graph, deontic gate tests).
        .filter(|code| code.starts_with("lifecycle.validation."))
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

/// LIFE-SOUND-5 holds of the effective transition: a subtype that appends a
/// guard to an inherited unguarded fact declares a guarded fact, even though
/// its own redeclaration names no mode.
#[test]
fn a_guard_appended_to_an_inherited_fact_is_a_guarded_fact() {
    let delivery = |name: &str, extends: Option<&str>, ship: serde_json::Value| {
        let mut value = serde_json::json!({
            "name": name,
            "lifecycles": [{
                "property": "status",
                "initial": ["pending"],
                "terminal": ["delivered"],
                "endings": [{ "state": "delivered", "outcome": "completed" }],
                "transitions": [
                    ship,
                    { "id": "deliver", "from": ["shipped"], "to": "delivered" }
                ]
            }]
        });
        if let Some(parent) = extends {
            value["extends"] = serde_json::json!(parent);
        } else {
            value["properties"] = serde_json::json!([
                { "id": "ref", "type": { "kind": "text" } },
                { "id": "status", "type": { "kind": "enum", "values": [
                    { "value": "pending" }, { "value": "shipped" }, { "value": "delivered" }
                ] }, "default": "pending" }
            ]);
            value["identity"] = serde_json::json!([{ "name": "primary", "properties": ["ref"] }]);
        }
        serde_json::from_value::<ObjectTypeDef>(value).expect("decodes")
    };
    let base = delivery(
        "test.Delivery",
        None,
        serde_json::json!({ "id": "ship", "from": ["pending"], "to": "shipped", "mode": "fact" }),
    );
    assert_eq!(run(std::slice::from_ref(&base), &[], None), Vec::new());
    let guarded = delivery(
        "test.CheckedDelivery",
        Some("test.Delivery"),
        serde_json::json!({
            "id": "ship", "from": ["pending"], "to": "shipped", "guards": ["self.ref != \"\""]
        }),
    );
    assert_eq!(
        run(&[guarded], &[base], None),
        vec![
            (
                "lifecycle.soundness.fact_guarded".to_owned(),
                "/objectTypes/0/lifecycles/0/transitions/0/guards".to_owned()
            ),
            // The only route to the ending is now guarded.
            (
                "lifecycle.soundness.completion_unknown".to_owned(),
                "/objectTypes/0/lifecycles/0".to_owned()
            )
        ]
    );
}

/// A subtype cannot turn an inherited fact into a request (so its appended
/// guard would make the base type's fact refusable), nor re-label an
/// inherited ending's outcome.
#[test]
fn a_subtype_keeps_an_inherited_mode_and_ending_outcome() {
    let base: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.Delivery",
        "properties": [
            { "id": "ref", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "pending" }, { "value": "shipped" }, { "value": "delivered" }
            ] }, "default": "pending" }
        ],
        "identity": [{ "name": "primary", "properties": ["ref"] }],
        "lifecycles": [{
            "property": "status",
            "initial": ["pending"],
            "terminal": ["delivered"],
            "endings": [{ "state": "delivered", "outcome": "completed" }],
            "transitions": [
                { "id": "ship", "from": ["pending"], "to": "shipped", "mode": "fact" },
                { "id": "deliver", "from": ["shipped"], "to": "delivered" }
            ]
        }]
    }))
    .expect("decodes");
    let subtype = |ship: serde_json::Value, outcome: &str| {
        serde_json::from_value::<ObjectTypeDef>(serde_json::json!({
            "name": "test.CheckedDelivery",
            "extends": "test.Delivery",
            "lifecycles": [{
                "property": "status",
                "initial": ["pending"],
                "terminal": ["delivered"],
                "endings": [{ "state": "delivered", "outcome": outcome }],
                "transitions": [
                    ship,
                    { "id": "deliver", "from": ["shipped"], "to": "delivered" }
                ]
            }]
        }))
        .expect("decodes")
    };
    let widens = |found: Vec<(String, String)>| {
        found
            .into_iter()
            .filter(|(code, _)| code == codes::WIDENS_TRANSITION)
            .map(|(_, at)| at)
            .collect::<Vec<_>>()
    };
    let request = subtype(
        serde_json::json!({
            "id": "ship", "from": ["pending"], "to": "shipped", "mode": "request",
            "guards": ["self.ref != \"\""]
        }),
        "completed",
    );
    assert_eq!(
        widens(run(&[request], std::slice::from_ref(&base), None)),
        vec!["/objectTypes/0/lifecycles/0/transitions/0/mode".to_owned()]
    );
    let relabelled = subtype(
        serde_json::json!({ "id": "ship", "from": ["pending"], "to": "shipped" }),
        "cancelled",
    );
    assert_eq!(
        widens(run(&[relabelled], std::slice::from_ref(&base), None)),
        vec!["/objectTypes/0/lifecycles/0/endings/0/outcome".to_owned()]
    );
    // Restating the inherited mode and outcome is accepted.
    let restated = subtype(
        serde_json::json!({ "id": "ship", "from": ["pending"], "to": "shipped", "mode": "fact" }),
        "completed",
    );
    assert_eq!(run(&[restated], &[base], None), Vec::new());
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
    assert_eq!(run(&[path_fixture("shipped", "paid")], &[], None), []);
    assert_eq!(run(&[path_fixture("shipped", "draft")], &[], None), []);
    assert_eq!(run(&[path_fixture("shipped", "shipped")], &[], None), []);
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
    assert_eq!(run(&types, &[], None), []);
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
    assert_eq!(run(&[parent, child], &[], None), []);
}

/// Every stuck state is its own finding: two unreachable states of one
/// lifecycle share the lifecycle's pointer, and the artifact set's dedup must
/// not fold the second into the first.
#[test]
fn every_unreachable_state_of_one_lifecycle_is_reported() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../registry/core/artifacts/commerce.SalesOrder.entity.json"
    );
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("sales order is readable"))
            .expect("sales order is JSON");
    // Without `invoice`, neither `invoiced` nor `completed` is reachable.
    value["payload"]["objectType"]["lifecycles"][0]["transitions"]
        .as_array_mut()
        .expect("transitions")
        .retain(|transition| transition["id"] != "invoice");
    let artifact = norn_checker::decode_artifact_json(&value).expect("sales order decodes");
    let findings = norn_checker::check_artifact_set(
        &[artifact],
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    let unreachable: Vec<&str> = findings
        .iter()
        .filter(|finding| finding.code.as_str() == codes::UNREACHABLE_STATE)
        .map(|finding| finding.message.as_str())
        .collect();
    assert_eq!(
        unreachable,
        [
            "state \"invoiced\" is not reachable from an initial state",
            "state \"completed\" is not reachable from an initial state",
        ]
    );
}

#[test]
fn guard_diagnostics_require_runtime_errors_and_retain_inherited_identity() {
    for code in [
        codes::PUBLICATION_GUARD,
        codes::UNKNOWN_STATE,
        "not.registered.code",
    ] {
        let mut object = purchase_order();
        object.lifecycles[0].transitions[0].guard_refusal_code = Some(code.to_owned());
        let refused = run(&[object], &[], None)
            .iter()
            .any(|(code, _)| code == codes::INVALID_GUARD_DIAGNOSTIC);
        assert_eq!(refused, code != codes::PUBLICATION_GUARD);
    }
    let mut base = purchase_order();
    let mut subtype = rush_purchase_order();
    let base_transition = &mut base.lifecycles[0].transitions[0];
    base_transition.guard_refusal_code = Some(codes::PUBLICATION_GUARD.to_owned());
    let transition = subtype.lifecycles[0]
        .transitions
        .iter_mut()
        .find(|transition| transition.id == base_transition.id)
        .expect("narrowed transition");
    transition.guard_refusal_code = Some("function.invocation.manifest_mismatch".to_owned());
    assert!(
        run(&[base, subtype], &[], None)
            .iter()
            .any(|(code, _)| code == codes::INVALID_GUARD_DIAGNOSTIC)
    );
}

// ---------------------------------------------------------------------------
// INTERFACE-LIFECYCLE-1
// ---------------------------------------------------------------------------

fn run_with_interfaces(
    interfaces: &[InterfaceDef],
    object_types: &[ObjectTypeDef],
    base_interfaces: &[InterfaceDef],
    base_types: &[ObjectTypeDef],
) -> Vec<(String, String)> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base: EntityModel {
            interfaces: base_interfaces,
            object_types: base_types,
            extensions: &[],
        },
        tenant: None,
    };
    let model = EntityModel {
        interfaces,
        object_types,
        extensions: &[],
    };
    pairs(check_entity_model(&model, &context))
}

fn commitment_interface() -> InterfaceDef {
    serde_json::from_value(serde_json::json!({
        "name": "core.Commitment",
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "proposed" },
                        { "value": "active" },
                        { "value": "fulfilled" },
                        { "value": "cancelled" }
                    ]
                }
            }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["proposed"],
            "terminal": ["fulfilled", "cancelled"],
            "immutable": ["fulfilled"],
            "endings": [
                { "state": "fulfilled", "outcome": "completed" },
                { "state": "cancelled", "outcome": "cancelled" }
            ],
            "pathProperties": [{ "target": "fulfilled", "via": "active" }],
            "transitions": [
                { "id": "activate", "from": ["proposed"], "to": "active", "guards": ["self.code != \"\""] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["proposed", "active"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("commitment interface decodes")
}

fn commitment_implementer(name: &str, extra: serde_json::Value) -> ObjectTypeDef {
    let mut obj = serde_json::json!({
        "name": name,
        "implements": ["core.Commitment"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "proposed" },
                        { "value": "active" },
                        { "value": "fulfilled" },
                        { "value": "cancelled" }
                    ]
                },
                "default": "proposed"
            }
        ],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    });
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            obj[k] = v.clone();
        }
    }
    serde_json::from_value(obj).expect("implementer decodes")
}

#[test]
fn interface_lifecycle_validates_standalone_and_enforces_life_sound() {
    let iface = commitment_interface();
    // Valid with zero implementors, including standalone local check.
    assert_eq!(
        run_with_interfaces(std::slice::from_ref(&iface), &[], &[], &[]),
        Vec::new()
    );
    assert_eq!(check_interface_local(&iface), Vec::new());

    // Unknown property, nullable property, non-enum property.
    let bad_prop: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.BadProp",
        "properties": [
            { "id": "flag", "type": { "kind": "boolean" } },
            {
                "id": "nullable_status",
                "type": { "kind": "enum", "values": [{ "value": "a" }, { "value": "b" }] },
                "constraints": { "nullable": true }
            }
        ],
        "lifecycles": [
            { "property": "missing", "initial": ["a"] },
            { "property": "flag", "initial": ["a"] },
            {
                "property": "nullable_status",
                "initial": ["a"],
                "terminal": ["b"],
                "transitions": [{ "id": "go", "from": ["a"], "to": "b" }]
            }
        ]
    }))
    .expect("decodes");
    let found = run_with_interfaces(&[bad_prop], &[], &[], &[]);
    assert!(found.contains(&(
        codes::UNKNOWN_PROPERTY.to_owned(),
        "/interfaces/0/lifecycles/0/property".to_owned()
    )));
    assert!(found.contains(&(
        codes::PROPERTY_NOT_ENUM.to_owned(),
        "/interfaces/0/lifecycles/1/property".to_owned()
    )));
    assert!(found.contains(&(
        codes::PROPERTY_NULLABLE.to_owned(),
        "/interfaces/0/lifecycles/2/property".to_owned()
    )));

    // LIFE-SOUND on interface: trap cycle and guarded fact.
    let unsound: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.Unsound",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [
                { "value": "a" }, { "value": "b" }, { "value": "done" }
            ] }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["a"],
            "terminal": ["done"],
            "endings": [{ "state": "done", "outcome": "completed" }],
            "transitions": [
                { "id": "loop_ab", "from": ["a"], "to": "b", "mode": "fact", "guards": ["true"] },
                { "id": "loop_ba", "from": ["b"], "to": "a" }
            ]
        }]
    }))
    .expect("decodes");
    let found = run_with_interfaces(&[unsound], &[], &[], &[]);
    assert!(
        found
            .iter()
            .any(|(code, _)| code == "lifecycle.soundness.fact_guarded")
    );
    assert!(
        found
            .iter()
            .any(|(code, _)| code == "lifecycle.soundness.trap")
    );
    assert!(
        found
            .iter()
            .any(|(code, _)| code == codes::UNREACHABLE_STATE)
    );
}

#[test]
fn implementer_inherits_interface_lifecycle_and_refuses_invalid_property_or_default() {
    let iface = commitment_interface();
    // Clean implementer without redeclaring lifecycle.
    let clean = commitment_implementer("sales.OrderCommitment", serde_json::json!({}));
    assert_eq!(
        run_with_interfaces(
            std::slice::from_ref(&iface),
            std::slice::from_ref(&clean),
            &[],
            &[]
        ),
        Vec::new()
    );

    // Non-initial default on implementer refuses.
    let bad_default = commitment_implementer(
        "sales.BadDefault",
        serde_json::json!({
            "properties": [
                { "id": "code", "type": { "kind": "text" } },
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [
                            { "value": "proposed" }, { "value": "active" },
                            { "value": "fulfilled" }, { "value": "cancelled" }
                        ]
                    },
                    "default": "active"
                }
            ]
        }),
    );
    assert_eq!(
        run_with_interfaces(std::slice::from_ref(&iface), &[bad_default], &[], &[]),
        vec![(
            codes::DEFAULT_NOT_INITIAL.to_owned(),
            "/objectTypes/0/properties/1".to_owned()
        )]
    );

    // Computed implementation of an interface lifecycle property refuses.
    let computed = commitment_implementer(
        "sales.ComputedStatus",
        serde_json::json!({
            "properties": [
                { "id": "code", "type": { "kind": "text" } },
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [
                            { "value": "proposed" }, { "value": "active" },
                            { "value": "fulfilled" }, { "value": "cancelled" }
                        ]
                    },
                    "computed": "\"proposed\""
                }
            ]
        }),
    );
    assert!(
        run_with_interfaces(std::slice::from_ref(&iface), &[computed], &[], &[])
            .iter()
            .any(|(code, at)| code == codes::PROPERTY_COMPUTED
                && at == "/objectTypes/0/implements/0")
    );

    // Enum narrowing without redeclaring lifecycle that drops initial state refuses.
    let drops_initial = commitment_implementer(
        "sales.NoInitial",
        serde_json::json!({
            "properties": [
                { "id": "code", "type": { "kind": "text" } },
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [{ "value": "active" }, { "value": "fulfilled" }]
                    }
                }
            ]
        }),
    );
    assert_eq!(
        run_with_interfaces(std::slice::from_ref(&iface), &[drops_initial], &[], &[]),
        vec![(
            codes::EMPTY_INITIAL.to_owned(),
            "/objectTypes/0/properties/1".to_owned()
        )]
    );

    // Enum narrowing without redeclaring lifecycle that leaves a dead end refuses.
    let dead_end = commitment_implementer(
        "sales.DeadEnd",
        serde_json::json!({
            "properties": [
                { "id": "code", "type": { "kind": "text" } },
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [{ "value": "proposed" }, { "value": "active" }]
                    },
                    "default": "proposed"
                }
            ]
        }),
    );
    assert!(
        run_with_interfaces(std::slice::from_ref(&iface), &[dead_end], &[], &[])
            .iter()
            .any(|(code, _)| code == codes::DEAD_END_STATE)
    );
}

#[test]
fn implementer_and_child_interface_narrowing_rules_match_object_inheritance() {
    let iface = commitment_interface();

    // Valid narrowing on implementer: drops `cancel` from `active`, appends guard on `activate`.
    let valid_narrow = commitment_implementer(
        "sales.StrictCommitment",
        serde_json::json!({
            "lifecycles": [{
                "property": "status",
                "initial": ["proposed"],
                "terminal": ["fulfilled", "cancelled"],
                "immutable": ["active", "fulfilled"],
                "endings": [
                    { "state": "fulfilled", "outcome": "completed" },
                    { "state": "cancelled", "outcome": "cancelled" }
                ],
                "transitions": [
                    { "id": "activate", "from": ["proposed"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) > 2"] },
                    { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                    { "id": "cancel", "from": ["proposed"], "to": "cancelled" }
                ]
            }]
        }),
    );
    assert_eq!(
        run_with_interfaces(std::slice::from_ref(&iface), &[valid_narrow], &[], &[]),
        Vec::new()
    );

    // Widening initial, adding transition, dropping guard, flipping mode, relabelling ending all refuse.
    let bad_narrow = commitment_implementer(
        "sales.WeakCommitment",
        serde_json::json!({
            "lifecycles": [{
                "property": "status",
                "initial": ["proposed", "active"],
                "terminal": ["fulfilled", "cancelled"],
                "endings": [
                    { "state": "fulfilled", "outcome": "compensated" },
                    { "state": "cancelled", "outcome": "cancelled" }
                ],
                "transitions": [
                    { "id": "activate", "from": ["proposed"], "to": "active", "guards": [] },
                    { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "request" },
                    { "id": "cancel", "from": ["proposed", "active"], "to": "cancelled" },
                    { "id": "reopen", "from": ["active"], "to": "proposed" }
                ]
            }]
        }),
    );
    let found = run_with_interfaces(std::slice::from_ref(&iface), &[bad_narrow], &[], &[]);
    assert!(found.iter().any(|(c, _)| c == codes::WIDENS_INITIAL));
    assert!(found.iter().any(|(c, _)| c == codes::DROPS_GUARD));
    assert!(
        found
            .iter()
            .any(|(c, at)| c == codes::WIDENS_TRANSITION && at.ends_with("/endings/0/outcome"))
    );
    assert!(
        found
            .iter()
            .any(|(c, at)| c == codes::WIDENS_TRANSITION && at.ends_with("/transitions/1/mode"))
    );
    assert!(
        found
            .iter()
            .any(|(c, at)| c == codes::WIDENS_TRANSITION && at.ends_with("/transitions/3/id"))
    );

    // Child interface narrowing parent interface: local check passes (property inherited from parent)
    // and model check rejects widening.
    let child_iface: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "sales.ChildCommitment",
        "extends": ["core.Commitment"],
        "lifecycles": [{
            "property": "status",
            "initial": ["proposed"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["proposed"], "to": "active", "guards": [] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["proposed"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("child interface decodes");
    assert_eq!(check_interface_local(&child_iface), Vec::new());
    let child_found = run_with_interfaces(&[iface, child_iface], &[], &[], &[]);
    assert!(child_found.contains(&(
        codes::DROPS_GUARD.to_owned(),
        "/interfaces/1/lifecycles/0/transitions/0/id".to_owned()
    )));
}

#[test]
fn interface_lifecycle_diamond_meets_cleanly_and_conflicts_fail_closed() {
    let base = commitment_interface();
    let left: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.LeftCommitment",
        "extends": ["core.Commitment"],
        "lifecycles": [{
            "property": "status",
            "initial": ["proposed"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["proposed"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) > 1"] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["proposed", "active"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("left decodes");
    let right: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.RightCommitment",
        "extends": ["core.Commitment"],
        "lifecycles": [{
            "property": "status",
            "initial": ["proposed"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["proposed"], "to": "active", "guards": ["self.code != \"\"", "len(self.code) < 50"] },
                { "id": "fulfill", "from": ["active"], "to": "fulfilled", "mode": "fact" },
                { "id": "cancel", "from": ["proposed"], "to": "cancelled" }
            ]
        }]
    }))
    .expect("right decodes");
    let join_obj: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.DiamondCommitment",
        "implements": ["test.LeftCommitment", "test.RightCommitment"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "proposed" }, { "value": "active" },
                        { "value": "fulfilled" }, { "value": "cancelled" }
                    ]
                },
                "default": "proposed"
            }
        ],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))
    .expect("diamond obj decodes");
    assert_eq!(
        run_with_interfaces(&[base.clone(), left, right], &[join_obj], &[], &[]),
        Vec::new()
    );

    // Conflicting interfaces: same transition id `fulfill` has incompatible target (`cancelled` vs `fulfilled`).
    let conflicting: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.ConflictingCommitment",
        "properties": [{
            "id": "status",
            "type": {
                "kind": "enum",
                "values": [
                    { "value": "proposed" }, { "value": "active" },
                    { "value": "fulfilled" }, { "value": "cancelled" }
                ]
            }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["proposed"],
            "terminal": ["fulfilled", "cancelled"],
            "transitions": [
                { "id": "activate", "from": ["proposed"], "to": "active" },
                { "id": "fulfill", "from": ["active"], "to": "cancelled", "mode": "fact" }
            ]
        }]
    }))
    .expect("conflicting decodes");
    let bad_obj: ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "test.ConflictedObj",
        "implements": ["core.Commitment", "test.ConflictingCommitment"],
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [
                        { "value": "proposed" }, { "value": "active" },
                        { "value": "fulfilled" }, { "value": "cancelled" }
                    ]
                },
                "default": "proposed"
            }
        ],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))
    .expect("bad obj decodes");
    let found = run_with_interfaces(&[base.clone(), conflicting.clone()], &[bad_obj], &[], &[]);
    assert!(found.iter().any(|(code, at)| code
        == norn_checker::object_types::codes::INTERFACE_REQUIREMENT_CONFLICT
        && at == "/objectTypes/0/implements/1"));

    let bad_iface: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.ConflictedIface",
        "extends": ["core.Commitment", "test.ConflictingCommitment"]
    }))
    .expect("bad iface decodes");
    let found_iface = run_with_interfaces(&[base, conflicting, bad_iface], &[], &[], &[]);
    assert!(found_iface.iter().any(|(code, at)| code
        == norn_checker::interfaces::codes::INVALID_INHERITANCE
        && at == "/interfaces/2/extends"));
}

#[test]
fn interface_lifecycle_sibling_narrowing_and_dangling_compensation_refuse() {
    // Sibling parent interface narrowing out every initial state is caught even
    // when the child interface declares no own properties or lifecycles.
    let iface_a: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.IfaceA",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "done" }] }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["done"],
            "transitions": [{ "id": "finish", "from": ["draft"], "to": "done" }]
        }]
    }))
    .expect("iface_a decodes");
    let iface_b: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.IfaceB",
        "properties": [{
            "id": "status",
            "type": { "kind": "enum", "values": [{ "value": "done" }] }
        }]
    }))
    .expect("iface_b decodes");
    let iface_c: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.IfaceC",
        "extends": ["test.IfaceA", "test.IfaceB"]
    }))
    .expect("iface_c decodes");
    let narrowed_out = run_with_interfaces(&[iface_a, iface_b, iface_c], &[], &[], &[]);
    assert!(
        narrowed_out
            .iter()
            .any(|(code, at)| code == codes::EMPTY_INITIAL && at == "/interfaces/2/extends")
    );

    // Diamond meet dropping a compensating transition fails closed.
    let comp_a: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.CompA",
        "properties": [{
            "id": "status",
            "type": {
                "kind": "enum",
                "values": [
                    { "value": "draft" }, { "value": "active" },
                    { "value": "done" }, { "value": "reversed" }
                ]
            }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["done", "reversed"],
            "transitions": [
                { "id": "charge", "from": ["draft"], "to": "active", "compensatedBy": "refund" },
                { "id": "complete", "from": ["active"], "to": "done" },
                { "id": "refund", "from": ["active"], "to": "reversed" }
            ],
            "endings": [
                { "state": "done", "outcome": "completed" },
                { "state": "reversed", "outcome": "compensated" }
            ]
        }]
    }))
    .expect("comp_a decodes");
    let comp_b: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.CompB",
        "properties": [{
            "id": "status",
            "type": {
                "kind": "enum",
                "values": [
                    { "value": "draft" }, { "value": "active" },
                    { "value": "done" }, { "value": "reversed" }
                ]
            }
        }],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["done", "reversed"],
            "transitions": [
                { "id": "charge", "from": ["draft"], "to": "active" },
                { "id": "complete", "from": ["active"], "to": "done" },
                { "id": "abandon", "from": ["draft"], "to": "reversed" }
            ],
            "endings": [
                { "state": "done", "outcome": "completed" },
                { "state": "reversed", "outcome": "compensated" }
            ]
        }]
    }))
    .expect("comp_b decodes");
    let comp_c: InterfaceDef = serde_json::from_value(serde_json::json!({
        "name": "test.CompC",
        "extends": ["test.CompA", "test.CompB"]
    }))
    .expect("comp_c decodes");
    let comp_conflict = run_with_interfaces(&[comp_a, comp_b, comp_c], &[], &[], &[]);
    assert!(comp_conflict.iter().any(|(code, at)| code
        == norn_checker::interfaces::codes::INVALID_INHERITANCE
        && at == "/interfaces/2/extends"));
}

#[test]
fn interface_lifecycle_diamond_lattice_over_cycle_stays_linear() {
    // A depth-25 two-wide diamond lattice stacked above a self-cyclic interface
    // would take ~2^25 visits without memoization across cyclic subtrees.
    let mut interfaces: Vec<InterfaceDef> = Vec::new();
    interfaces.push(
        serde_json::from_value(serde_json::json!({
            "name": "test.CycleRoot",
            "extends": ["test.CycleRoot"]
        }))
        .expect("cycle root decodes"),
    );
    let mut prev = vec!["test.CycleRoot".to_owned()];
    for level in 0..25_u32 {
        let left = format!("test.L{level}A");
        let right = format!("test.L{level}B");
        for name in [&left, &right] {
            interfaces.push(
                serde_json::from_value(serde_json::json!({
                    "name": name,
                    "extends": prev
                }))
                .expect("lattice node decodes"),
            );
        }
        prev = vec![left, right];
    }
    let found = run_with_interfaces(&interfaces, &[], &[], &[]);
    assert!(
        found
            .iter()
            .any(|(code, _)| code == norn_checker::interfaces::codes::INVALID_INHERITANCE),
        "cycle must still be reported: {found:?}"
    );
}
