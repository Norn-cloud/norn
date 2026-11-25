//! PRINCIPAL-INPUT-1 (#1091): the shared catalog, the PDP-decided
//! `holdsRole` and the fail-closed resolver.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue;
use norn_ir::access::{AccessIr, PrincipalAttributeCatalog, ScopeDimension};
use norn_ir::rule::PrincipalInputDeclaration;
use norn_projections::CedarPrincipalAttribute;
use serde_json::{Value, json};

use crate::{
    ActorKind, AttrValue, AuthzCode, CapabilityGrant, Delegation, HOLDS_ROLE_ACTION, HoldsRolePdp,
    HoldsRolePolicy, PrincipalContext, PrincipalInput, ScopeKey, admit_principal_catalog,
    holds_role_keys, principal_attribute_catalog, resolve_principal_inputs, scope_dimensions,
};

fn scope(org: &str) -> ScopeKey {
    ScopeKey {
        org: org.to_owned(),
        tenant: None,
        workspace: None,
        environment: Some("dev".to_owned()),
        business_unit: None,
    }
}

fn access(entries: &Value) -> AccessIr {
    serde_json::from_value(json!({
        "accessVersion": "0.1", "policyId": "review.access", "rules": [],
        "principalCatalog": entries
    }))
    .expect("access policy")
}

fn catalog() -> PrincipalAttributeCatalog {
    let entries = json!([
        {"name": "reviewer", "type": "bool", "source": {"kind": "role"}},
        {"name": "editors", "type": "stringSet",
         "source": {"kind": "roleProfile", "roles": ["reviewer", "publisher"]}},
        {"name": "id", "type": "string", "source": {"kind": "attribute"}},
        {"name": "level", "type": "long", "source": {"kind": "attribute"}},
        {"name": "regions", "type": "stringSet", "source": {"kind": "attribute"}}
    ]);
    principal_attribute_catalog(&[access(&entries)], &BTreeMap::new()).expect("catalog")
}

fn inputs(declared: Value) -> Vec<PrincipalInputDeclaration> {
    serde_json::from_value(declared).expect("principal inputs")
}

fn declared() -> Vec<PrincipalInputDeclaration> {
    inputs(json!([
        {"name": "isReviewer", "kind": {"holdsRole": {"role": "reviewer", "at": "resource"}}},
        {"name": "reviewsHere", "kind": {"holdsRole": {"role": "reviewer", "at": ["org", "environment"]}}},
        {"name": "editorRoles", "kind": {"roleProfile": {"profile": "editors"}}},
        {"name": "subject", "kind": {"attribute": {"name": "id"}}},
        {"name": "level", "kind": {"attribute": {"name": "level"}}},
        {"name": "regions", "kind": {"attribute": {"name": "regions"}}}
    ]))
}

fn principal(subject: &str, kind: ActorKind, roles: &[&str]) -> PrincipalInput {
    PrincipalInput {
        subject_id: subject.to_owned(),
        actor_kind: kind,
        agent_session_id: (kind == ActorKind::Agent).then(|| "session-1".to_owned()),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        attributes: BTreeMap::from([
            ("id".to_owned(), AttrValue::String(subject.to_owned())),
            ("level".to_owned(), AttrValue::Long(3)),
            (
                "regions".to_owned(),
                AttrValue::Set(vec![
                    AttrValue::String("north".to_owned()),
                    AttrValue::String("east".to_owned()),
                ]),
            ),
        ]),
    }
}

/// The PDP holding `who`'s capability grant in `org`.
fn pdp(who: &PrincipalInput, org: &str, declared: &[PrincipalInputDeclaration]) -> HoldsRolePdp {
    let keys = holds_role_keys(&catalog(), declared, &scope_dimensions(&scope(org)));
    HoldsRolePolicy::compile(&keys)
        .expect("holdsRole policy")
        .pdp(&[CapabilityGrant {
            subject_id: who.subject_id.clone(),
            actor_kind: who.actor_kind,
            agent_session_id: who.agent_session_id.clone(),
            scope: Some(scope(org)),
            capabilities: vec![HOLDS_ROLE_ACTION.to_owned()],
            binding: None,
        }])
        .expect("pdp")
}

fn resolve(
    who: &PrincipalInput,
    subject_org: &str,
    delegation: &Delegation,
) -> Result<BTreeMap<String, RuntimeValue>, String> {
    let declared = declared();
    let request_scope = scope("org_demo");
    let subject_scope = scope(subject_org);
    resolve_principal_inputs(
        &catalog(),
        &pdp(who, "org_demo", &declared),
        &PrincipalContext {
            principal: who,
            request_scope: &request_scope,
            subject_scope: &subject_scope,
            delegation,
        },
        &declared,
    )
    .map_err(|unavailable| unavailable.input)
}

fn strings(values: &[&str]) -> RuntimeValue {
    RuntimeValue::Array(
        values
            .iter()
            .map(|value| RuntimeValue::String((*value).to_owned()))
            .collect(),
    )
}

#[test]
fn every_declared_kind_resolves_to_its_catalog_type_for_the_acting_principal() {
    let reviewer = principal("user:rana", ActorKind::User, &["reviewer", "member"]);
    let resolved = resolve(&reviewer, "org_demo", &Delegation::Direct).expect("resolved");
    assert_eq!(
        resolved,
        BTreeMap::from([
            ("isReviewer".to_owned(), RuntimeValue::Bool(true)),
            ("reviewsHere".to_owned(), RuntimeValue::Bool(true)),
            ("editorRoles".to_owned(), strings(&["reviewer"])),
            (
                "subject".to_owned(),
                RuntimeValue::String("user:rana".into())
            ),
            ("level".to_owned(), RuntimeValue::Number(3_i64.into())),
            ("regions".to_owned(), strings(&["east", "north"])),
        ])
    );
    // A principal without the role gets `false` and an empty profile: a value, not a refusal.
    let member = principal("user:omar", ActorKind::User, &["member"]);
    let resolved = resolve(&member, "org_demo", &Delegation::Direct).expect("resolved");
    assert_eq!(resolved["isReviewer"], RuntimeValue::Bool(false));
    assert_eq!(resolved["editorRoles"], strings(&[]));
    assert_eq!(
        resolved["subject"],
        RuntimeValue::String("user:omar".into())
    );
}

#[test]
fn a_role_is_decided_at_the_subjects_scope_by_the_pdp() {
    let reviewer = principal("user:rana", ActorKind::User, &["reviewer"]);
    // The subject lives in another scope: the role is not held there, while
    // the same role at the request scope's own dimensions still is.
    let resolved = resolve(&reviewer, "org_other", &Delegation::Direct).expect("resolved");
    assert_eq!(resolved["isReviewer"], RuntimeValue::Bool(false));
    assert_eq!(resolved["reviewsHere"], RuntimeValue::Bool(true));
}

#[test]
fn a_delegated_agent_resolves_as_the_narrower_delegated_principal() {
    // The agent acts for a publisher but was delegated only `member`: it
    // reads its own roles, never its delegator's.
    let agent = principal("agent:drafter", ActorKind::Agent, &["member"]);
    let resolved = resolve(&agent, "org_demo", &Delegation::Direct).expect("resolved");
    assert_eq!(resolved["isReviewer"], RuntimeValue::Bool(false));
    assert_eq!(resolved["editorRoles"], strings(&[]));
    assert_eq!(
        resolved["subject"],
        RuntimeValue::String("agent:drafter".into())
    );
    // A presented delegation token cannot be verified yet: nothing resolves.
    let token = Delegation::Biscuit("token".to_owned());
    assert_eq!(
        resolve(&agent, "org_demo", &token),
        Err("isReviewer".to_owned())
    );
}

#[test]
fn an_input_that_cannot_be_resolved_is_unavailable_and_names_only_the_input() {
    let reviewer = principal("user:rana", ActorKind::User, &["reviewer"]);
    let declared = declared();
    let request_scope = scope("org_demo");
    let context = PrincipalContext {
        principal: &reviewer,
        request_scope: &request_scope,
        subject_scope: &request_scope,
        delegation: &Delegation::Direct,
    };
    // No capability grant in this scope: the PDP cannot decide the role.
    let ungranted = pdp(&reviewer, "org_other", &declared);
    let refused = resolve_principal_inputs(&catalog(), &ungranted, &context, &declared)
        .expect_err("unavailable");
    assert_eq!(refused.input, "isReviewer");
    // A role pair the host never compiled is not silently `false`.
    let uncompiled = pdp(&reviewer, "org_demo", &declared[3..]);
    let refused = resolve_principal_inputs(&catalog(), &uncompiled, &context, &declared)
        .expect_err("unavailable");
    assert_eq!(refused.input, "isReviewer");
    // A scoped attribute the mapper did not supply, or supplied at another
    // type, has no default; the refusal never repeats the value.
    let granted = pdp(&reviewer, "org_demo", &declared);
    for attributes in [
        BTreeMap::new(),
        BTreeMap::from([("id".to_owned(), AttrValue::Long(41_772))]),
    ] {
        let mut unmapped = reviewer.clone();
        unmapped.attributes = attributes;
        let context = PrincipalContext {
            principal: &unmapped,
            ..context
        };
        let refused = resolve_principal_inputs(&catalog(), &granted, &context, &declared[3..])
            .expect_err("unavailable");
        assert_eq!(refused.input, "subject");
        assert!(!refused.to_string().contains("41772"));
    }
    // An input the catalog does not define.
    let unknown = inputs(json!([
        {"name": "isOwner", "kind": {"holdsRole": {"role": "owner", "at": "resource"}}}
    ]));
    let refused = resolve_principal_inputs(&catalog(), &granted, &context, &unknown)
        .expect_err("unavailable");
    assert_eq!(refused.input, "isOwner");
}

#[test]
fn the_catalog_is_one_with_the_types_access_rules_infer() {
    let declared =
        json!([{"name": "department", "type": "string", "source": {"kind": "attribute"}}]);
    // `AccessIR` uses the same attribute as a string: one catalog.
    let agreeing = BTreeMap::from([("department".to_owned(), CedarPrincipalAttribute::String)]);
    let catalog =
        principal_attribute_catalog(&[access(&declared)], &agreeing).expect("one catalog");
    assert!(catalog.get("department").is_some());
    // `AccessIR` uses it as a set: the declaration refuses.
    let disagreeing =
        BTreeMap::from([("department".to_owned(), CedarPrincipalAttribute::StringSet)]);
    let error = principal_attribute_catalog(&[access(&declared)], &disagreeing)
        .expect_err("one name, two types");
    assert!(error.has(AuthzCode::AccessPrincipalConflict), "{error}");
    // Two policies declaring one name differently refuse; identically, they agree.
    let other = json!([{"name": "department", "type": "long", "source": {"kind": "attribute"}}]);
    let error = principal_attribute_catalog(&[access(&declared), access(&other)], &BTreeMap::new())
        .expect_err("conflicting declarations");
    assert!(error.has(AuthzCode::AccessPrincipalConflict), "{error}");
    assert!(
        principal_attribute_catalog(&[access(&declared), access(&declared)], &BTreeMap::new())
            .is_ok()
    );
    // A role is a boolean; an attribute name is an identifier.
    for invalid in [
        json!([{"name": "reviewer", "type": "long", "source": {"kind": "role"}}]),
        json!([{"name": "cost-centre", "type": "string", "source": {"kind": "attribute"}}]),
    ] {
        let error = principal_attribute_catalog(&[access(&invalid)], &BTreeMap::new())
            .expect_err("invalid entry");
        assert!(error.has(AuthzCode::AccessNameInvalid), "{error}");
    }
}

#[test]
fn a_listed_scope_must_be_present_and_compiled_pairs_are_exact() {
    let declared = inputs(json!([
        {"name": "inTenant", "kind": {"holdsRole": {"role": "reviewer", "at": ["org", "tenant"]}}}
    ]));
    let keys = holds_role_keys(&catalog(), &declared, &scope_dimensions(&scope("org_demo")));
    assert_eq!(
        keys.into_iter().collect::<Vec<_>>(),
        [(
            "reviewer".to_owned(),
            vec![ScopeDimension::Org, ScopeDimension::Tenant]
        )]
    );
    // The request scope carries no tenant: the role is not held at a
    // dimension the request does not have.
    let reviewer = principal("user:rana", ActorKind::User, &["reviewer"]);
    let request_scope = scope("org_demo");
    let resolved = resolve_principal_inputs(
        &catalog(),
        &pdp(&reviewer, "org_demo", &declared),
        &PrincipalContext {
            principal: &reviewer,
            request_scope: &request_scope,
            subject_scope: &request_scope,
            delegation: &Delegation::Direct,
        },
        &declared,
    )
    .expect("resolved");
    assert_eq!(resolved["inTenant"], RuntimeValue::Bool(false));
}

/// S4: the catalog a definition set declares is checked against what the
/// set's own access rules use each principal attribute at, before install.
#[test]
fn admission_refuses_a_catalog_its_own_access_rules_contradict() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "review.Document",
        "properties": [{"id": "owner", "type": {"kind": "text"}}]
    }))
    .expect("object type");
    // The rule compares a text property with the principal's `id`.
    let policy = |catalog: Value| -> AccessIr {
        serde_json::from_value(json!({
            "accessVersion": "0.1", "policyId": "review.access",
            "rules": [{"ruleId": "owners", "actions": ["review.approve"],
                "resourceType": "review.Document", "scope": {"dimensions": ["org"]},
                "allOf": [{"resource": {"name": "owner", "op": "eq",
                    "value": {"principalAttribute": "id"}}}]}],
            "principalCatalog": catalog
        }))
        .expect("access policy")
    };
    let admit = |catalog: Value| {
        admit_principal_catalog(
            &[policy(catalog)],
            std::slice::from_ref(&object_type),
            &[],
            &[],
        )
    };
    let agreed = admit(json!([{"name": "id", "type": "string", "source": {"kind": "attribute"}}]))
        .expect("the declared type is the inferred one");
    assert!(agreed.get("id").is_some());
    let error = admit(json!([{"name": "id", "type": "long", "source": {"kind": "attribute"}}]))
        .expect_err("id is used as a string");
    assert!(error.has(AuthzCode::AccessPrincipalConflict), "{error}");
    let error = admit(json!([
        {"name": "cost-center", "type": "string", "source": {"kind": "attribute"}}
    ]))
    .expect_err("not an identifier");
    assert!(error.has(AuthzCode::AccessNameInvalid), "{error}");
}
