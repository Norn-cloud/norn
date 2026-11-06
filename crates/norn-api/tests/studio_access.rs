//! The hosted Studio role policy (#817), decided by the real `AccessIR` → Cedar
//! PDP: a viewer reads, a member also proposes, an admin or owner also
//! approves. Everything else, including another organization's workspace, an
//! unknown role or a missing capability grant, denies.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "policy setup must succeed before decision assertions"
)]
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PdpDecision,
    PrincipalInput, ResourceInput, ScopeKey,
};
use std::collections::BTreeMap;

const ACTIONS: [&str; 3] = ["studio.read", "studio.propose", "studio.approve"];

fn org(id: &str) -> ScopeKey {
    ScopeKey {
        org: id.into(),
        tenant: None,
        workspace: None,
        environment: None,
        business_unit: None,
    }
}

fn policy() -> norn_ir::access::AccessIr {
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_str(include_str!("fixtures/studio-access.json")).unwrap();
    let payload = artifacts.first().unwrap().payload.to_json_value();
    serde_json::from_value(serde_json::to_value(payload).unwrap()).unwrap()
}

/// The PDP with the compiled policy and one subject's capability grant in its own org.
fn pdp(subject: &str, capabilities: &[&str]) -> Pdp {
    let (entities, actions) = include_str!("fixtures/studio-access.cedarschema")
        .split_once("\n\n")
        .unwrap();
    let schema = AuthzSchema::from_projected_cedarschema_fragments(&[entities, actions]).unwrap();
    Pdp::new(
        authz::compile(&schema, &policy()).unwrap(),
        &[CapabilityGrant {
            subject_id: subject.into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(org("org_a")),
            capabilities: capabilities.iter().map(|c| (*c).into()).collect(),
            binding: None,
        }],
    )
    .unwrap()
}

fn request(subject: &str, role: &str, action: &str, workspace_org: &str) -> AccessRequest {
    AccessRequest {
        principal: PrincipalInput {
            subject_id: subject.into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![role.into()],
            attributes: BTreeMap::new(),
        },
        scope: Some(org("org_a")),
        capability: action.into(),
        action_id: action.into(),
        resource: ResourceInput {
            object_type: "studio.Workspace".into(),
            id: "main".into(),
            scope: org(workspace_org),
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}

fn allowed(pdp: &Pdp, role: &str, action: &str) -> bool {
    matches!(
        pdp.decide(&request("user:u1", role, action, "org_a")),
        PdpDecision::Allow
    )
}

#[test]
fn each_role_may_do_exactly_its_actions() {
    let pdp = pdp("user:u1", &ACTIONS);
    let expected = [
        ("viewer", [true, false, false]),
        ("member", [true, true, false]),
        ("admin", [true, true, true]),
        ("owner", [true, true, true]),
    ];
    for (role, decisions) in expected {
        for (action, want) in ACTIONS.iter().zip(decisions) {
            assert_eq!(allowed(&pdp, role, action), want, "{role} {action}");
        }
    }
}

#[test]
fn unknown_roles_and_other_organizations_deny() {
    let pdp = pdp("user:u1", &ACTIONS);
    for action in ACTIONS {
        assert!(!allowed(&pdp, "superuser", action), "unknown role {action}");
        assert!(!allowed(&pdp, "", action), "empty role {action}");
        let foreign = request("user:u1", "owner", action, "org_b");
        assert!(
            !matches!(pdp.decide(&foreign), PdpDecision::Allow),
            "foreign org {action}"
        );
    }
}

#[test]
fn a_role_without_the_hosts_capability_grant_denies() {
    let reads_only = pdp("user:u1", &["studio.read"]);
    assert!(allowed(&reads_only, "admin", "studio.read"));
    assert!(!allowed(&reads_only, "admin", "studio.approve"));
    let other_subject = pdp("user:someone-else", &ACTIONS);
    assert!(!allowed(&other_subject, "admin", "studio.read"));
}
