//! Integration of `AccessIR` lowering with the `EntityIR` Cedar projection.

use std::collections::BTreeMap;
use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityUid, Request};
use norn_ir::{
    ObjectTypeName,
    access::AccessIr,
    entitlement::{Entitlement, EntitlementCondition, EntitlementGrantee, ObjectRef},
};
use norn_projections::{CedarAttribute, CedarLoss, CedarPrincipalAttribute, CedarSchemaProjection};
use serde_json::{Value, json};

use crate::{
    AccessRequest, ActorKind, AttrValue, AuthzCode, AuthzSchema, CapabilityGrant, Delegation,
    EntitlementCatalog, EntitlementDenialReason, EntitlementInput, EntitlementRowOutcome,
    EntitlementValidationError, Pdp, PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
    compile_all, compile_all_projected, compile_all_projected_with_entitlements,
    infer_principal_attributes, validate_entitlement,
};

const PROJECTED_SCHEMA: &str = r#"
namespace commerce {
  entity Order = {
    scope: Norn::Scope,
    acted_by?: Set<Norn::Actor>,
    salesperson?: Norn::EntityRef,
    watchers?: Set<Norn::EntityRef>,
    total?: Long,
    enabled?: Bool,
    state?: String,
    business_unit?: String,
    price?: String
  };
  entity SpecialOrder in [Order] = { scope: Norn::Scope };
}
namespace Norn {
  type EntityRef = { type: String, key: String };
  entity Actor in [Role] = {
    scope: Scope,
    represents?: EntityRef
  };
  action "norn.read" appliesTo {
    principal: [Actor],
    resource: [commerce::Order, commerce::SpecialOrder],
    context: { scope: Scope }
  };
  action "orders.read" appliesTo {
    principal: [Actor],
    resource: [commerce::Order, commerce::SpecialOrder],
    context: { scope: Scope }
  };
}
"#;

fn projection() -> CedarSchemaProjection {
    CedarSchemaProjection {
        schema: PROJECTED_SCHEMA.to_owned(),
        losses: Vec::<CedarLoss>::new(),
        attributes: BTreeMap::from([
            (
                "commerce.Order".to_owned(),
                BTreeMap::from([
                    (
                        "salesperson".to_owned(),
                        CedarAttribute::Link { many: false },
                    ),
                    ("watchers".to_owned(), CedarAttribute::Link { many: true }),
                    ("total".to_owned(), CedarAttribute::Long),
                    ("enabled".to_owned(), CedarAttribute::Bool),
                    ("state".to_owned(), CedarAttribute::String),
                    ("business_unit".to_owned(), CedarAttribute::String),
                    ("price".to_owned(), CedarAttribute::Decimal),
                ]),
            ),
            (
                "commerce.SpecialOrder".to_owned(),
                BTreeMap::from([
                    ("total".to_owned(), CedarAttribute::Long),
                    ("enabled".to_owned(), CedarAttribute::Bool),
                    ("state".to_owned(), CedarAttribute::String),
                    ("business_unit".to_owned(), CedarAttribute::String),
                    ("price".to_owned(), CedarAttribute::Decimal),
                    (
                        "salesperson".to_owned(),
                        CedarAttribute::Link { many: false },
                    ),
                    ("watchers".to_owned(), CedarAttribute::Link { many: true }),
                ]),
            ),
            (
                "Norn::Actor".to_owned(),
                BTreeMap::from([(
                    "represents".to_owned(),
                    CedarAttribute::Link { many: false },
                )]),
            ),
        ]),
        principal_attributes: BTreeMap::new(),
        object_types: vec![
            ObjectTypeName("commerce.Order".to_owned()),
            ObjectTypeName("commerce.SpecialOrder".to_owned()),
        ],
        resource_actions: BTreeMap::from([
            (
                ObjectTypeName("commerce.Order".to_owned()),
                vec!["orders.read".to_owned()],
            ),
            (
                ObjectTypeName("commerce.SpecialOrder".to_owned()),
                vec!["orders.read".to_owned()],
            ),
        ]),
    }
}

fn projection_with_principal_attributes(
    principal_attributes: BTreeMap<String, CedarPrincipalAttribute>,
) -> CedarSchemaProjection {
    let mut projection = projection();
    let fields = principal_attributes
        .iter()
        .map(|(name, attribute)| {
            let cedar_type = match attribute {
                CedarPrincipalAttribute::Bool => "Bool",
                CedarPrincipalAttribute::Long => "Long",
                CedarPrincipalAttribute::String => "String",
                CedarPrincipalAttribute::BoolSet => "Set<Bool>",
                CedarPrincipalAttribute::LongSet => "Set<Long>",
                CedarPrincipalAttribute::StringSet => "Set<String>",
            };
            format!("    {name}?: {cedar_type}")
        })
        .collect::<Vec<_>>();
    let actor = if fields.is_empty() {
        "  entity Actor in [Role] = {\n    scope: Scope,\n    represents?: EntityRef\n  };"
            .to_owned()
    } else {
        format!(
            "  entity Actor in [Role] = {{\n    scope: Scope,\n    represents?: EntityRef,\n{}\n  }};",
            fields.join(",\n")
        )
    };
    projection.schema = projection.schema.replace(
        "  entity Actor in [Role] = {\n    scope: Scope,\n    represents?: EntityRef\n  };",
        &actor,
    );
    projection.principal_attributes = principal_attributes;
    projection
}

fn schema(projection: &CedarSchemaProjection) -> AuthzSchema {
    let json = AuthzSchema::projected_json(&projection.schema)
        .expect("projected Cedar schema serializes to JSON");
    AuthzSchema::from_projected_json(&json).expect("JSON schema composes")
}

fn access(all_of: Vec<Value>) -> AccessIr {
    access_for("commerce.Order", all_of)
}

fn access_for(resource_type: &str, all_of: Vec<Value>) -> AccessIr {
    serde_json::from_value(json!({
        "accessVersion": "0.1",
        "policyId": "orders.access",
        "rules": [{
            "ruleId": "read",
            "actions": ["orders.read"],
            "resourceType": resource_type,
            "scope": { "dimensions": ["org"] },
            "allOf": Value::Array(all_of)
        }]
    }))
    .expect("AccessIR decodes")
}

fn projected_policies(all_of: Vec<Value>) -> (CedarSchemaProjection, crate::ValidatedPolicySet) {
    let base = projection();
    let access = access(all_of);
    let attributes = infer_principal_attributes(std::slice::from_ref(&access), &base)
        .expect("principal attribute types infer from projected resource properties");
    let projection = projection_with_principal_attributes(attributes);
    let policies = compile_all_projected(&schema(&projection), &[access], &projection)
        .expect("policies compile against the inferred Actor schema");
    (projection, policies)
}

fn uid(text: &str) -> EntityUid {
    EntityUid::from_str(text).expect("entity uid")
}

fn pdp_scope(org: &str) -> ScopeKey {
    ScopeKey {
        org: org.to_owned(),
        tenant: None,
        workspace: None,
        environment: None,
        business_unit: None,
    }
}

fn pdp_request(
    principal_attributes: BTreeMap<String, AttrValue>,
    resource_business_unit: &str,
    resource_scope: ScopeKey,
    roles: &[&str],
) -> AccessRequest {
    let scope = pdp_scope("o1");
    AccessRequest {
        principal: PrincipalInput {
            subject_id: "alice".to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            attributes: principal_attributes,
        },
        scope: Some(scope),
        capability: "orders.read".to_owned(),
        action_id: "orders.read".to_owned(),
        resource: ResourceInput {
            object_type: "commerce.Order".to_owned(),
            id: "order-1".to_owned(),
            scope: resource_scope,
            attributes: BTreeMap::from([(
                "business_unit".to_owned(),
                AttrValue::String(resource_business_unit.to_owned()),
            )]),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}

fn pdp(policies: crate::ValidatedPolicySet) -> Pdp {
    let grant = CapabilityGrant {
        subject_id: "alice".to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(pdp_scope("o1")),
        capabilities: vec!["orders.read".to_owned()],
        binding: None,
    };
    Pdp::new(policies, &[grant]).expect("PDP compiles")
}

fn entitlement_pdp() -> Pdp {
    pdp(entitlement_policy_set())
}

fn entitlement_policy_set() -> crate::ValidatedPolicySet {
    let base = projection();
    let access: AccessIr = serde_json::from_value(json!({
        "accessVersion": "0.1",
        "policyId": "core.entitlements",
        "rules": [],
        "genericRules": [
            {
                "ruleId": "entitlement.none",
                "target": { "kind": "allAdmittedObjectTypes" },
                "actions": { "kind": "allAdmittedActions" },
                "scope": { "dimensions": ["org"] },
                "allOf": [{ "entitlement": { "kind": "none" } }]
            },
            {
                "ruleId": "entitlement.owner_only",
                "target": { "kind": "allAdmittedObjectTypes" },
                "actions": { "kind": "allAdmittedActions" },
                "scope": { "dimensions": ["org"] },
                "allOf": [{ "entitlement": { "kind": "ownerOnly" } }]
            },
            {
                "ruleId": "entitlement.linked_record",
                "target": { "kind": "allAdmittedObjectTypes" },
                "actions": { "kind": "allAdmittedActions" },
                "scope": { "dimensions": ["org"] },
                "allOf": [{ "entitlement": { "kind": "linkedRecord" } }]
            }
        ]
    }))
    .expect("the three fixed generic AccessIR rules decode");
    let attributes = infer_principal_attributes(std::slice::from_ref(&access), &base)
        .expect("generic rules infer the reserved typed sets");
    let projection = projection_with_principal_attributes(attributes);
    let catalog = EntitlementCatalog::from_projection(&projection);
    assert_eq!(
        catalog.resource_actions[&ObjectTypeName("commerce.Order".to_owned())],
        vec!["norn.read".to_owned(), "orders.read".to_owned()]
    );
    let policies = compile_all_projected(&schema(&projection), &[access], &projection)
        .expect("generic entitlement rules expand against admitted pairs");
    assert_eq!(policies.len(), 6, "three templates × two admitted types");
    policies
}

#[test]
fn generic_catalog_is_bound_to_the_admitted_projection() {
    let base = projection();
    let access: AccessIr = serde_json::from_value(json!({
        "accessVersion": "0.1",
        "policyId": "core.entitlements",
        "rules": [],
        "genericRules": [{
            "ruleId": "entitlement.none",
            "target": { "kind": "allAdmittedObjectTypes" },
            "actions": { "kind": "allAdmittedActions" },
            "scope": { "dimensions": ["org"] },
            "allOf": [{ "entitlement": { "kind": "none" } }]
        }]
    }))
    .expect("generic entitlement AccessIR decodes");
    let attributes = infer_principal_attributes(std::slice::from_ref(&access), &base)
        .expect("generic rule infers matcher signal type");
    let projection = projection_with_principal_attributes(attributes);
    let schema = schema(&projection);
    let catalog = EntitlementCatalog::from_projection(&projection);
    assert!(
        compile_all_projected_with_entitlements(
            &schema,
            std::slice::from_ref(&access),
            &projection,
            &catalog,
        )
        .is_ok()
    );

    let widened_catalog = EntitlementCatalog {
        resource_actions: BTreeMap::new(),
    };
    let error =
        compile_all_projected_with_entitlements(&schema, &[access], &projection, &widened_catalog)
            .expect_err("catalogs that diverge from admitted projection are refused");
    assert!(error.has(AuthzCode::AccessGenericRuleInvalid));
}

fn entitlement_input(status: &str, role_key: &str, business_unit: &str) -> EntitlementInput {
    let entitlement: Entitlement = serde_json::from_value(json!({
        "entitlement_version": "0.1",
        "id": "erpnext:DocPerm:42",
        "grantee": {
            "kind": "role",
            "objectType": "identity.Role",
            "key": role_key
        },
        "target_type": "commerce.Order",
        "actions": ["orders.read"],
        "scope_attributes": { "business_unit": business_unit },
        "condition": { "kind": "none" },
        "source": {
            "system": "erpnext",
            "recordId": "DocPerm/42",
            "version": "17.0"
        },
        "status": status
    }))
    .expect("test entitlement decodes");
    EntitlementInput {
        entitlement,
        scope: pdp_scope("o1"),
    }
}

fn principal_with_role_key(role_key: &str) -> BTreeMap<String, AttrValue> {
    BTreeMap::from([(
        "entitlement_roles".to_owned(),
        AttrValue::Set(vec![AttrValue::String(role_key.to_owned())]),
    )])
}

#[allow(
    clippy::too_many_arguments,
    reason = "test matrix varies independent optional Cedar attributes"
)]
fn decide(
    policies: &crate::ValidatedPolicySet,
    represents: Option<Value>,
    salesperson: Option<Value>,
    watchers: Vec<Value>,
    acted_by: &[&str],
    total: Option<i64>,
    enabled: bool,
    state: &str,
) -> Decision {
    let schema = policies.schema().cedar();
    let mut actor_attributes = json!({ "scope": { "org": "o1" } });
    if let Some(represents) = represents {
        actor_attributes["represents"] = represents;
    }
    let mut order_attributes = json!({
        "scope": { "org": "o1" },
        "enabled": enabled,
        "state": state,
        "watchers": Value::Array(watchers),
    });
    if let Some(total) = total {
        order_attributes["total"] = json!(total);
    }
    if let Some(salesperson) = salesperson {
        order_attributes["salesperson"] = salesperson;
    }
    let actor_refs: Vec<Value> = acted_by
        .iter()
        .map(|actor| json!({ "__entity": { "type": "Norn::Actor", "id": actor } }))
        .collect();
    if !actor_refs.is_empty() {
        order_attributes["acted_by"] = Value::Array(actor_refs);
    }
    let mut entities = vec![json!({
        "uid": { "type": "Norn::Actor", "id": "sales_agent" },
        "attrs": actor_attributes,
        "parents": []
    })];
    for actor in acted_by.iter().filter(|actor| **actor != "sales_agent") {
        entities.push(json!({
            "uid": { "type": "Norn::Actor", "id": actor },
            "attrs": { "scope": { "org": "o1" } },
            "parents": []
        }));
    }
    entities.push(json!({
        "uid": { "type": "commerce::Order", "id": "so1" },
        "attrs": order_attributes,
        "parents": []
    }));
    let entities = Entities::from_json_str(&Value::Array(entities).to_string(), Some(schema))
        .expect("entities conform to projected schema");
    let action = uid(r#"Norn::Action::"orders.read""#);
    let context = Context::from_json_str(r#"{"scope":{"org":"o1"}}"#, Some((schema, &action)))
        .expect("context conforms to projected schema");
    let request = Request::new(
        uid(r#"Norn::Actor::"sales_agent""#),
        action,
        uid(r#"commerce::Order::"so1""#),
        context,
        Some(schema),
    )
    .expect("request conforms to projected schema");
    Authorizer::new()
        .is_authorized(&request, policies.cedar_policies(), &entities)
        .decision()
}

fn record(key: &str) -> Value {
    json!({ "type": "party.Person", "key": key })
}

fn rel(role: &str) -> Value {
    json!({ "rel": { "from": "resource", "role": role } })
}

#[test]
fn projected_resource_comparisons_are_typed_and_evaluate() {
    let projection = projection();
    let access = access(vec![
        json!({ "resource": { "name": "total", "op": "gt", "value": 10 } }),
        json!({ "resource": { "name": "enabled", "op": "eq", "value": true } }),
        json!({ "resource": { "name": "state", "op": "eq", "value": "open" } }),
    ]);
    let policies = compile_all_projected(&schema(&projection), &[access], &projection)
        .expect("declared scalar properties compile");
    assert!(
        policies
            .to_cedar()
            .contains(r#"resource has "total" && resource["total"] > 10"#)
    );
    assert_eq!(
        decide(&policies, None, None, vec![], &[], Some(11), true, "open"),
        Decision::Allow
    );
    assert_eq!(
        decide(&policies, None, None, vec![], &[], Some(10), true, "open"),
        Decision::Deny
    );
    assert_eq!(
        decide(&policies, None, None, vec![], &[], None, true, "open"),
        Decision::Deny
    );
}

#[test]
fn resource_comparisons_cannot_bypass_projection_or_compare_links() {
    let condition = json!({ "resource": { "name": "total", "op": "gt", "value": 10 } });
    let error = compile_all(&schema(&projection()), &[access(vec![condition])])
        .expect_err("a resource comparison without projection metadata is refused");
    assert!(error.has(AuthzCode::AccessProjectionRequired));

    let projection = projection();
    for property in ["missing", "salesperson"] {
        let error = compile_all_projected(
            &schema(&projection),
            &[access(vec![json!({
                "resource": { "name": property, "op": "eq", "value": "x" }
            })])],
            &projection,
        )
        .expect_err("unknown and link attributes are not scalar comparisons");
        assert!(error.has(AuthzCode::AccessNameInvalid));
    }
}

#[test]
fn every_decimal_comparison_is_refused_with_the_stable_code() {
    let projection = projection();
    for op in ["eq", "ne", "lt", "le", "gt", "ge"] {
        let error = compile_all_projected(
            &schema(&projection),
            &[access(vec![json!({
                "resource": { "name": "price", "op": op, "value": "10.00" }
            })])],
            &projection,
        )
        .expect_err("decimal comparisons are not order-safe after string projection");
        assert!(error.has(AuthzCode::AccessDecimalComparison), "{op}");
        assert_eq!(
            AuthzCode::AccessDecimalComparison.as_str(),
            "authz.access.decimal_comparison"
        );

        let inherited_error = compile_all_projected(
            &schema(&projection),
            &[access_for(
                "commerce.SpecialOrder",
                vec![json!({
                    "resource": { "name": "price", "op": op, "value": "10.00" }
                })],
            )],
            &projection,
        )
        .expect_err("inherited decimal properties remain non-comparable");
        assert!(
            inherited_error.has(AuthzCode::AccessDecimalComparison),
            "{op}"
        );
    }
}

#[test]
fn principal_attribute_types_infer_from_resource_scalars_and_projection_must_match() {
    let base = projection();
    let documents = [access(vec![
        json!({
            "resource": {
                "name": "business_unit",
                "op": "eq",
                "value": { "principalAttribute": "business_unit" }
            }
        }),
        json!({
            "resource": {
                "name": "business_unit",
                "op": "in",
                "value": { "principalAttribute": "business_units" }
            }
        }),
    ])];
    assert_eq!(
        infer_principal_attributes(&documents, &base).expect("inference succeeds"),
        BTreeMap::from([
            ("business_unit".to_owned(), CedarPrincipalAttribute::String),
            (
                "business_units".to_owned(),
                CedarPrincipalAttribute::StringSet
            ),
        ])
    );

    let unaugmented_schema = schema(&base);
    let error = compile_all_projected(&unaugmented_schema, &documents, &base)
        .expect_err("the caller must project the inferred actor fields");
    assert!(error.has(AuthzCode::AccessPrincipalProjectionRequired));
}

#[test]
fn principal_attribute_admission_rejects_conflicts_reserved_names_and_unsupported_types() {
    let base = projection();
    let conflict = [
        access(vec![json!({
            "resource": { "name": "business_unit", "op": "eq", "value": { "principalAttribute": "unit" } }
        })]),
        access(vec![json!({
            "resource": { "name": "business_unit", "op": "in", "value": { "principalAttribute": "unit" } }
        })]),
    ];
    let error = infer_principal_attributes(&conflict, &base).expect_err("cardinality conflicts");
    assert!(error.has(AuthzCode::AccessPrincipalConflict));

    for name in ["scope", "represents", "acted_by"] {
        let access = access(vec![json!({
            "resource": { "name": "business_unit", "op": "eq", "value": { "principalAttribute": name } }
        })]);
        let error = infer_principal_attributes(&[access], &base)
            .expect_err("reserved actor members are refused");
        assert!(error.has(AuthzCode::AccessPrincipalReserved), "{name}");
    }

    let invalid_name = access(vec![json!({
        "resource": { "name": "business_unit", "op": "eq", "value": { "principalAttribute": "bad-name" } }
    })]);
    assert!(
        infer_principal_attributes(&[invalid_name], &base)
            .expect_err("invalid name")
            .has(AuthzCode::AccessNameInvalid)
    );

    let decimal = access(vec![json!({
        "resource": { "name": "price", "op": "eq", "value": { "principalAttribute": "prices" } }
    })]);
    assert!(
        infer_principal_attributes(&[decimal], &base)
            .expect_err("decimal comparisons are refused")
            .has(AuthzCode::AccessDecimalComparison)
    );

    let link = access(vec![json!({
        "resource": { "name": "salesperson", "op": "eq", "value": { "principalAttribute": "people" } }
    })]);
    assert!(
        infer_principal_attributes(&[link], &base)
            .expect_err("links are refused")
            .has(AuthzCode::AccessNameInvalid)
    );

    let bad_operator = access(vec![json!({
        "resource": { "name": "business_unit", "op": "gt", "value": { "principalAttribute": "unit" } }
    })]);
    assert!(
        infer_principal_attributes(&[bad_operator], &base)
            .expect_err("ordering principal values is unsupported")
            .has(AuthzCode::AccessPrincipalOperator)
    );

    let literal_in = access(vec![json!({
        "resource": { "name": "business_unit", "op": "in", "value": "sales" }
    })]);
    let error = compile_all_projected(&schema(&base), &[literal_in], &base)
        .expect_err("`in` requires a principal attribute set");
    assert!(error.has(AuthzCode::AccessResourceOperator));
}

#[test]
fn pdp_principal_scalar_equality_requires_a_present_matching_typed_attribute() {
    let (_, policies) = projected_policies(vec![json!({
        "resource": {
            "name": "business_unit",
            "op": "eq",
            "value": { "principalAttribute": "business_unit" }
        }
    })]);
    let pdp = pdp(policies);
    let allow = pdp_request(
        BTreeMap::from([(
            "business_unit".to_owned(),
            AttrValue::String("sales".to_owned()),
        )]),
        "sales",
        pdp_scope("o1"),
        &[],
    );
    assert_eq!(pdp.decide(&allow), PdpDecision::Allow);

    let mismatch = pdp_request(
        BTreeMap::from([(
            "business_unit".to_owned(),
            AttrValue::String("finance".to_owned()),
        )]),
        "sales",
        pdp_scope("o1"),
        &[],
    );
    assert_eq!(
        pdp.decide(&mismatch),
        PdpDecision::Deny(crate::DenyReason::Access)
    );

    let missing = pdp_request(BTreeMap::new(), "sales", pdp_scope("o1"), &[]);
    assert_eq!(
        pdp.decide(&missing),
        PdpDecision::Deny(crate::DenyReason::Access)
    );

    let invalid_type = pdp_request(
        BTreeMap::from([("business_unit".to_owned(), AttrValue::Long(42))]),
        "sales",
        pdp_scope("o1"),
        &[],
    );
    assert_eq!(
        pdp.decide(&invalid_type),
        PdpDecision::Deny(crate::DenyReason::AccessError)
    );
}

#[test]
fn pdp_membership_accepts_any_principal_business_unit_and_keeps_scope_mandatory() {
    let (_, policies) = projected_policies(vec![
        json!({ "role": { "anyOf": ["Reader"] } }),
        json!({
            "resource": {
                "name": "business_unit",
                "op": "in",
                "value": { "principalAttribute": "business_units" }
            }
        }),
    ]);
    let pdp = pdp(policies);
    let units = AttrValue::Set(vec![
        AttrValue::String("finance".to_owned()),
        AttrValue::String("sales".to_owned()),
    ]);
    let allowed = pdp_request(
        BTreeMap::from([("business_units".to_owned(), units)]),
        "sales",
        pdp_scope("o1"),
        &["Reader"],
    );
    assert_eq!(pdp.decide(&allowed), PdpDecision::Allow);

    let empty = pdp_request(
        BTreeMap::from([("business_units".to_owned(), AttrValue::Set(Vec::new()))]),
        "sales",
        pdp_scope("o1"),
        &["Reader"],
    );
    assert_eq!(
        pdp.decide(&empty),
        PdpDecision::Deny(crate::DenyReason::Access)
    );

    let missing = pdp_request(BTreeMap::new(), "sales", pdp_scope("o1"), &["Reader"]);
    assert_eq!(
        pdp.decide(&missing),
        PdpDecision::Deny(crate::DenyReason::Access)
    );

    let invalid_type = pdp_request(
        BTreeMap::from([(
            "business_units".to_owned(),
            AttrValue::String("sales".to_owned()),
        )]),
        "sales",
        pdp_scope("o1"),
        &["Reader"],
    );
    assert_eq!(
        pdp.decide(&invalid_type),
        PdpDecision::Deny(crate::DenyReason::AccessError)
    );

    let wrong_scope = pdp_request(
        BTreeMap::from([(
            "business_units".to_owned(),
            AttrValue::Set(vec![AttrValue::String("sales".to_owned())]),
        )]),
        "sales",
        pdp_scope("o2"),
        &["Reader"],
    );
    assert_eq!(
        pdp.decide(&wrong_scope),
        PdpDecision::Deny(crate::DenyReason::Access),
        "a matching role and principal attribute cannot bypass the structural scope conjunct"
    );
}

#[test]
fn projected_single_and_many_relations_compare_records_and_require_binding() {
    let projection = projection();
    let single = compile_all_projected(
        &schema(&projection),
        &[access(vec![rel("salesperson")])],
        &projection,
    )
    .expect("singleton link compiles");
    assert!(single.to_cedar().contains(
        r#"resource has "salesperson" && principal has "represents" && resource["salesperson"] == principal["represents"]"#
    ));
    assert_eq!(
        decide(
            &single,
            Some(record("p1")),
            Some(record("p1")),
            vec![],
            &[],
            None,
            false,
            "closed"
        ),
        Decision::Allow
    );
    assert_eq!(
        decide(
            &single,
            None,
            Some(record("p1")),
            vec![],
            &[],
            None,
            false,
            "closed"
        ),
        Decision::Deny
    );
    assert_eq!(
        decide(
            &single,
            Some(record("p2")),
            Some(record("p1")),
            vec![],
            &[],
            None,
            false,
            "closed"
        ),
        Decision::Deny
    );

    let many = compile_all_projected(
        &schema(&projection),
        &[access(vec![rel("watchers")])],
        &projection,
    )
    .expect("set-valued link compiles");
    assert!(many.to_cedar().contains(
        r#"resource has "watchers" && principal has "represents" && resource["watchers"].contains(principal["represents"])"#
    ));
    assert_eq!(
        decide(
            &many,
            Some(record("p1")),
            None,
            vec![record("p2"), record("p1")],
            &[],
            None,
            false,
            "closed"
        ),
        Decision::Allow
    );
    assert_eq!(
        decide(
            &many,
            Some(record("p1")),
            None,
            vec![record("p2")],
            &[],
            None,
            false,
            "closed"
        ),
        Decision::Deny
    );
}

#[test]
fn projected_rel_rejects_unknown_non_link_and_principal_side_roles() {
    let projection = projection();
    let unknown = compile_all_projected(
        &schema(&projection),
        &[access(vec![rel("unknown")])],
        &projection,
    )
    .expect_err("unknown link is refused");
    assert!(unknown.has(AuthzCode::AccessNameInvalid));

    let scalar = compile_all_projected(
        &schema(&projection),
        &[access(vec![rel("total")])],
        &projection,
    )
    .expect_err("scalar property is not a link");
    assert!(scalar.has(AuthzCode::AccessNameInvalid));

    let principal_side = access(vec![json!({
        "rel": { "from": "principal", "role": "accounts" }
    })]);
    let unsupported = compile_all_projected(&schema(&projection), &[principal_side], &projection)
        .expect_err("projected record bindings do not support principal-side traversal");
    assert!(unsupported.has(AuthzCode::AccessRelUnsupported));
}

#[test]
fn projected_acted_by_keeps_the_2950_actor_set_semantics() {
    let projection = projection();
    let policies = compile_all_projected(
        &schema(&projection),
        &[access(vec![rel("acted_by")])],
        &projection,
    )
    .expect("host relation compiles");
    assert!(
        policies
            .to_cedar()
            .contains(r#"resource has "acted_by" && principal in resource["acted_by"]"#)
    );
    assert_eq!(
        decide(
            &policies,
            None,
            None,
            vec![],
            &["sales_agent"],
            None,
            false,
            "closed"
        ),
        Decision::Allow
    );
    assert_eq!(
        decide(&policies, None, None, vec![], &[], None, false, "closed"),
        Decision::Deny
    );
    assert_eq!(
        decide(
            &policies,
            None,
            None,
            vec![],
            &["other"],
            None,
            false,
            "closed"
        ),
        Decision::Deny
    );
}

#[test]
fn actual_entity_projection_validates_and_round_trips_as_cedar_json() {
    let objects = serde_json::from_value::<Vec<norn_ir::ObjectTypeDef>>(json!([
        {"name":"hr.Employee", "properties":[{"id":"name", "type":{"kind":"text"}}]},
        {"name":"commerce.Order", "properties":[
            {"id":"amount", "type":{"kind":"ref", "name":"money.amount"}},
            {"id":"enabled", "type":{"kind":"boolean"}},
            {"id":"count", "type":{"kind":"integer"}}
        ], "links":[{"id":"salesperson", "target":"hr.Employee",
            "cardinality":{"to":"one", "from":"many"}, "inverse":"orders"}]},
        {"name":"commerce.SpecialOrder", "extends":"commerce.Order", "properties":[]}
    ]))
    .expect("objects");
    let catalog = serde_json::from_value::<Vec<norn_ir::PropertyTypeDef>>(json!([
        {"name":"money.amount", "type":{"kind":"decimal", "scale":2}}
    ]))
    .expect("catalog");
    let projection =
        norn_projections::project_cedar_schema_with_catalog(&objects, &[], &catalog, &[]);
    let projected_json = AuthzSchema::projected_json(&projection.schema).expect("schema JSON");
    AuthzSchema::from_projected_json(&projected_json).expect("schema validates");
    assert_eq!(
        projection
            .attributes
            .get("commerce.SpecialOrder")
            .and_then(|attributes| attributes.get("amount")),
        Some(&CedarAttribute::Decimal)
    );
    assert_eq!(
        projection
            .attributes
            .get("hr.Employee")
            .and_then(|attributes| attributes.get("orders")),
        Some(&CedarAttribute::Link { many: true })
    );
}

#[test]
fn generic_entitlement_policies_deny_when_reserved_evidence_is_absent() {
    let policies = entitlement_policy_set();
    assert_eq!(
        decide(&policies, None, None, Vec::new(), &[], None, true, "draft"),
        Decision::Deny,
        "a projected actor without entitlement evidence must safely default to deny"
    );
}

#[test]
fn enforced_entitlements_drive_the_three_fixed_cedar_templates() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let row = entitlement_input("enforced", "role-7", "finance");
    let explanation = pdp.explain_with_entitlements(&request, &[row]);

    assert_eq!(explanation.decision, PdpDecision::Allow);
    assert_eq!(
        explanation.entitlement_evaluation.outcome,
        crate::ConjunctOutcome::Allow
    );
    assert_eq!(explanation.entitlement_evaluation.enforced_matches.len(), 1);
    assert!(
        explanation
            .determining_policies
            .iter()
            .any(|policy| { policy.rule_id.as_deref() == Some("entitlement.none") })
    );
}

#[test]
fn shadow_entitlements_never_grant_but_explain_the_enforced_counterfactual() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let shadow = entitlement_input("shadow", "role-7", "finance");
    let explanation = pdp.explain_with_entitlements(&request, &[shadow]);

    assert_eq!(
        explanation.decision,
        PdpDecision::Deny(crate::DenyReason::Access)
    );
    assert!(
        explanation
            .entitlement_evaluation
            .enforced_matches
            .is_empty()
    );
    assert_eq!(explanation.entitlement_evaluation.shadow_matches.len(), 1);
    assert_eq!(
        explanation.entitlement_evaluation.rows[0].outcome,
        EntitlementRowOutcome::ShadowMatch
    );
    assert_eq!(
        explanation.entitlement_evaluation.would_allow_if_enforced,
        Some(true)
    );
}

#[test]
fn shadow_counterfactual_keeps_capability_delegation_and_access_errors() {
    let policies = entitlement_policy_set();
    let no_capability = Pdp::new(policies, &[]).expect("empty grants compile");
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let shadow = entitlement_input("shadow", "role-7", "finance");
    let capability_denied =
        no_capability.explain_with_entitlements(&request, std::slice::from_ref(&shadow));
    assert_eq!(
        capability_denied.decision,
        PdpDecision::Deny(crate::DenyReason::Capability)
    );
    assert_eq!(
        capability_denied.entitlement_evaluation.outcome,
        crate::ConjunctOutcome::NotEvaluated
    );
    assert_eq!(
        capability_denied
            .entitlement_evaluation
            .would_allow_if_enforced,
        Some(false)
    );

    let pdp = entitlement_pdp();
    let mut delegated_request = request.clone();
    delegated_request.delegation = Delegation::Biscuit("unverified".to_owned());
    let delegated =
        pdp.explain_with_entitlements(&delegated_request, std::slice::from_ref(&shadow));
    assert_eq!(
        delegated.decision,
        PdpDecision::Deny(crate::DenyReason::Delegation)
    );
    assert_eq!(
        delegated.entitlement_evaluation.outcome,
        crate::ConjunctOutcome::NotEvaluated
    );
    assert_eq!(
        delegated.entitlement_evaluation.would_allow_if_enforced,
        Some(false)
    );

    let mut invalid_resource = request;
    // Preserve the matching entitlement scope while making Cedar reject a
    // different projected string attribute.
    invalid_resource
        .resource
        .attributes
        .insert("state".to_owned(), AttrValue::Long(42));
    let errored = pdp.explain_with_entitlements(&invalid_resource, &[shadow]);
    assert_eq!(errored.entitlement_evaluation.shadow_matches.len(), 1);
    assert_eq!(errored.access, crate::ConjunctOutcome::Error);
    assert_eq!(
        errored.entitlement_evaluation.would_allow_if_enforced,
        Some(false)
    );
}

#[test]
fn unrelated_shadow_rows_do_not_create_a_counterfactual_or_hide_target_evidence() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let mut rows: Vec<_> = (0..70)
        .map(|index| {
            let mut row = entitlement_input("shadow", "role-7", "finance");
            row.entitlement.id = format!("other-{index:03}");
            row.entitlement.target_type = ObjectTypeName("app.Other".to_owned());
            row
        })
        .collect();
    let unrelated = pdp.explain_with_entitlements(&request, &rows);
    assert_eq!(
        unrelated.entitlement_evaluation.would_allow_if_enforced,
        None
    );
    let mut matching = entitlement_input("shadow", "role-7", "finance");
    matching.entitlement.id = "target-last".to_owned();
    rows.push(matching);
    let relevant = pdp.explain_with_entitlements(&request, &rows);
    assert_eq!(
        relevant.entitlement_evaluation.would_allow_if_enforced,
        Some(true)
    );
    assert_eq!(relevant.entitlement_evaluation.rows[0].id, "target-last");
    assert_eq!(relevant.entitlement_evaluation.omitted_rows, 7);
}

#[test]
fn entitlement_rows_never_widen_across_type_action_scope_or_grantee() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    assert!(
        pdp.decide_with_entitlements(
            &request,
            &[entitlement_input("enforced", "role-7", "finance")]
        )
        .is_allow()
    );

    // Every non-empty combination of independent mismatches must still deny.
    for mask in 1_u8..32 {
        let mut row = entitlement_input("enforced", "role-7", "finance");
        if mask & 1 != 0 {
            row.entitlement.target_type = ObjectTypeName("commerce.SpecialOrder".to_owned());
        }
        if mask & 2 != 0 {
            row.entitlement.actions = vec!["orders.write".to_owned()];
        }
        if mask & 4 != 0 {
            row.entitlement
                .scope_attributes
                .insert("business_unit".to_owned(), "operations".to_owned());
        }
        if mask & 8 != 0 {
            row.entitlement.grantee = EntitlementGrantee::Role {
                object_type: ObjectTypeName("identity.Role".to_owned()),
                key: "another-role".to_owned(),
            };
        }
        if mask & 16 != 0 {
            row.scope = pdp_scope("other-org");
        }
        assert_eq!(
            pdp.decide_with_entitlements(&request, &[row]),
            PdpDecision::Deny(crate::DenyReason::Access),
            "mismatch mask {mask:#07b} widened authorization"
        );
    }
}

#[test]
fn entitlement_role_and_group_keys_are_typed_and_distinct_from_role_names() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let mut role_row = entitlement_input("enforced", "role-7", "finance");
    assert!(validate_entitlement(&role_row).is_ok());
    assert_eq!(
        pdp.decide_with_entitlements(&request, std::slice::from_ref(&role_row)),
        PdpDecision::Allow
    );

    let mut group_row = entitlement_input("enforced", "ignored-role", "finance");
    group_row.entitlement.grantee = EntitlementGrantee::Group {
        object_type: ObjectTypeName("identity.Group".to_owned()),
        key: "group-4".to_owned(),
    };
    let group_request = pdp_request(
        BTreeMap::from([(
            "entitlement_groups".to_owned(),
            AttrValue::Set(vec![AttrValue::String("group-4".to_owned())]),
        )]),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    assert_eq!(
        pdp.decide_with_entitlements(&group_request, &[group_row]),
        PdpDecision::Allow
    );

    role_row.entitlement.grantee = EntitlementGrantee::Role {
        object_type: ObjectTypeName("party.Person".to_owned()),
        key: "role-7".to_owned(),
    };
    assert_eq!(
        validate_entitlement(&role_row),
        Err(EntitlementValidationError::GranteeTypeInvalid)
    );
    let role_name_only = pdp_request(BTreeMap::new(), "finance", pdp_scope("o1"), &["role-7"]);
    let valid_role_row = entitlement_input("enforced", "role-7", "finance");
    assert_eq!(
        pdp.decide_with_entitlements(&role_name_only, &[valid_role_row]),
        PdpDecision::Deny(crate::DenyReason::Access)
    );

    let mut principal_row = entitlement_input("enforced", "ignored-role", "finance");
    principal_row.entitlement.grantee = EntitlementGrantee::Principal {
        object_type: ObjectTypeName("party.Person".to_owned()),
        key: "alice".to_owned(),
    };
    let principal_request = pdp_request(
        BTreeMap::from([(
            "represents".to_owned(),
            AttrValue::Record(BTreeMap::from([
                (
                    "type".to_owned(),
                    AttrValue::String("party.Person".to_owned()),
                ),
                ("key".to_owned(), AttrValue::String("alice".to_owned())),
            ])),
        )]),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    assert_eq!(
        pdp.decide_with_entitlements(&principal_request, &[principal_row]),
        PdpDecision::Allow
    );
}

#[test]
fn explain_reports_bounded_related_row_reasons_and_provenance() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let row = entitlement_input("enforced", "role-7", "operations");
    let explanation = pdp.explain_with_entitlements(&request, &[row]);

    assert_eq!(
        explanation.decision,
        PdpDecision::Deny(crate::DenyReason::Access)
    );
    assert_eq!(explanation.entitlement_evaluation.rows.len(), 1);
    let evidence = &explanation.entitlement_evaluation.rows[0];
    assert_eq!(evidence.source_system, "erpnext");
    assert_eq!(evidence.source_record_id, "DocPerm/42");
    assert_eq!(
        evidence.outcome,
        EntitlementRowOutcome::Mismatch(EntitlementDenialReason::ScopeAttributeMismatch)
    );
    assert_eq!(
        EntitlementDenialReason::ScopeAttributeMismatch.code(),
        "authz.entitlement.scope_attribute_mismatch"
    );
}

#[test]
fn owner_and_linked_record_conditions_compare_typed_link_references() {
    let pdp = entitlement_pdp();
    let owner_ref = AttrValue::Record(BTreeMap::from([
        (
            "type".to_owned(),
            AttrValue::String("party.Person".to_owned()),
        ),
        ("key".to_owned(), AttrValue::String("alice".to_owned())),
    ]));
    let mut owner_attributes = principal_with_role_key("role-7");
    owner_attributes.insert("represents".to_owned(), owner_ref.clone());
    let mut owner_request = pdp_request(owner_attributes, "finance", pdp_scope("o1"), &[]);
    owner_request
        .resource
        .attributes
        .insert("salesperson".to_owned(), owner_ref.clone());

    let mut owner_row = entitlement_input("enforced", "role-7", "finance");
    owner_row.entitlement.condition = EntitlementCondition::OwnerOnly {
        link: norn_ir::LinkId("salesperson".to_owned()),
    };
    assert_eq!(
        pdp.decide_with_entitlements(&owner_request, std::slice::from_ref(&owner_row)),
        PdpDecision::Allow
    );
    owner_request.resource.attributes.insert(
        "salesperson".to_owned(),
        AttrValue::Record(BTreeMap::from([
            (
                "type".to_owned(),
                AttrValue::String("party.Person".to_owned()),
            ),
            ("key".to_owned(), AttrValue::String("bob".to_owned())),
        ])),
    );
    let denied = pdp.explain_with_entitlements(&owner_request, &[owner_row]);
    assert_eq!(
        denied.decision,
        PdpDecision::Deny(crate::DenyReason::Access)
    );
    assert_eq!(
        denied.entitlement_evaluation.rows[0].outcome,
        EntitlementRowOutcome::Mismatch(EntitlementDenialReason::OwnerMismatch)
    );

    let mut linked_request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    linked_request
        .resource
        .attributes
        .insert("watchers".to_owned(), AttrValue::Set(vec![owner_ref]));
    let mut linked_row = entitlement_input("enforced", "role-7", "finance");
    linked_row.entitlement.condition = EntitlementCondition::LinkedRecord {
        link: norn_ir::LinkId("watchers".to_owned()),
        target: ObjectRef {
            object_type: ObjectTypeName("party.Person".to_owned()),
            key: "alice".to_owned(),
        },
    };
    assert_eq!(
        pdp.decide_with_entitlements(&linked_request, &[linked_row]),
        PdpDecision::Allow
    );
}

#[test]
fn callers_cannot_inject_the_reserved_entitlement_match_signal() {
    let pdp = entitlement_pdp();
    let mut principal = principal_with_role_key("role-7");
    principal.insert(
        "entitlement_kinds".to_owned(),
        AttrValue::Set(vec![AttrValue::String("none".to_owned())]),
    );
    let request = pdp_request(principal, "finance", pdp_scope("o1"), &[]);
    assert_eq!(
        pdp.decide_with_entitlements(&request, &[]),
        PdpDecision::Deny(crate::DenyReason::InvalidRequest)
    );
}

#[test]
fn entitlement_membership_attributes_do_not_change_legacy_cedar_schemas() {
    let (projection, policies) = projected_policies(vec![json!({
        "role": { "anyOf": ["approver"] }
    })]);
    assert!(projection.principal_attributes.is_empty());
    let pdp = pdp(policies);
    let request = pdp_request(
        principal_with_role_key("some-role-key"),
        "finance",
        pdp_scope("o1"),
        &["approver"],
    );
    assert_eq!(pdp.decide(&request), PdpDecision::Allow);
}

#[test]
fn standalone_access_rules_cannot_use_entitlement_conditions_or_reserved_sets() {
    let projection = projection();
    let entitlement_rule = access(vec![json!({ "entitlement": { "kind": "none" } })]);
    let error = compile_all_projected(&schema(&projection), &[entitlement_rule], &projection)
        .expect_err("only the fixed generic templates may contain entitlement predicates");
    assert!(error.has(AuthzCode::AccessGenericRuleInvalid));

    let access = access(vec![json!({
        "resource": {
            "name": "business_unit",
            "op": "in",
            "value": { "principalAttribute": "entitlement_roles" }
        }
    })]);
    let error = infer_principal_attributes(&[access], &projection)
        .expect_err("matcher-only role keys are reserved from authored AccessIR");
    assert!(error.has(AuthzCode::AccessPrincipalReserved));
}

#[test]
fn explain_bounds_related_row_diagnostics_and_reports_omissions() {
    let pdp = entitlement_pdp();
    let request = pdp_request(
        principal_with_role_key("role-7"),
        "finance",
        pdp_scope("o1"),
        &[],
    );
    let rows: Vec<_> = (0..70)
        .map(|index| {
            let mut row = entitlement_input("enforced", "role-7", "finance");
            row.entitlement.id = format!("erpnext:DocPerm:{index:02}");
            row.entitlement.source.record_id = format!("DocPerm/{index:02}");
            row
        })
        .collect();
    let explanation = pdp.explain_with_entitlements(&request, &rows);
    assert_eq!(explanation.entitlement_evaluation.rows.len(), 64);
    assert_eq!(
        explanation.entitlement_evaluation.enforced_matches.len(),
        64
    );
    assert_eq!(explanation.entitlement_evaluation.omitted_rows, 6);
    let reversed: Vec<_> = rows.into_iter().rev().collect();
    let reversed_explanation = pdp.explain_with_entitlements(&request, &reversed);
    assert_eq!(
        explanation.entitlement_evaluation.rows,
        reversed_explanation.entitlement_evaluation.rows
    );
    assert_eq!(
        explanation.entitlement_evaluation.enforced_matches,
        reversed_explanation.entitlement_evaluation.enforced_matches
    );
}
