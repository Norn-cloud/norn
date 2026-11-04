//! DIAG-DISCLOSE-1: actual scoped AccessIR/Cedar decisions, never endpoint grants.
#![allow(
    clippy::unwrap_used,
    reason = "policy setup must succeed before refusal assertions"
)]
use super::*;
use crate::{authz::DenyAll, scope::CellScope};
use norn_kernel::authz::{
    ActorKind, AuthzSchema, CapabilityGrant, Delegation, PrincipalInput, ResourceInput,
};
use std::collections::BTreeMap;

fn scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}
fn key() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        environment: Some("dev".into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}
fn policy() -> norn_ir::access::AccessIr {
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_str(include_str!("../tests/fixtures/refusal-access.json")).unwrap();
    let policy = artifacts.first().unwrap().payload.to_json_value();
    serde_json::from_value(serde_json::to_value(policy).unwrap()).unwrap()
}
fn pdp(capabilities: &[&str], declared: bool) -> Pdp {
    let (entities, host) = include_str!("../tests/fixtures/refusal-access.cedarschema")
        .split_once("\n\n")
        .unwrap();
    let schema = AuthzSchema::from_projected_cedarschema_fragments(&[entities, host]).unwrap();
    let mut policy = policy();
    if !declared {
        policy.rules.clear();
    }
    Pdp::new(
        authz::compile(&schema, &policy).unwrap(),
        &[CapabilityGrant {
            subject_id: "principal:operator".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(key()),
            capabilities: capabilities.iter().map(|s| (*s).into()).collect(),
            binding: None,
        }],
    )
    .unwrap()
}
fn claims(request: &RefusalDetailRequest<'_>) -> Option<authz::AccessRequest> {
    if request.actor != "principal:operator" {
        return None;
    }
    let scope = ScopeKey {
        org: request.scope.org_id.clone(),
        environment: Some(request.scope.environment_id.clone()),
        ..key()
    };
    Some(authz::AccessRequest {
        principal: PrincipalInput {
            subject_id: request.actor.into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec!["Operator".into()],
            attributes: BTreeMap::new(),
        },
        scope: Some(scope.clone()),
        capability: DISCLOSE_REFUSAL_DETAIL_ACTION.into(),
        action_id: DISCLOSE_REFUSAL_DETAIL_ACTION.into(),
        resource: ResourceInput {
            object_type: REFUSAL_DETAIL_RESOURCE.into(),
            id: request.action_id.into(),
            scope,
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::from([(
            "governedAction".into(),
            AttrValue::String(request.action_id.into()),
        )]),
        binding: None,
        delegation: Delegation::Direct,
    })
}
fn refused() -> norn_kernel::Refusal {
    let mut source = norn_kernel::Refusal::new(
        "core.laws.violated",
        norn_kernel::RefusalCategory::Precondition,
        "the proposed quantity violates the retained law",
    );
    source.reason = Some(norn_ir::RefusalReason {
        law: "demo.nonnegative".into(),
        subject: "inventory.CycleHolding/R1".into(),
        values: BTreeMap::from([(
            "quantity".into(),
            norn_ir::RuntimeValue::String("-1.000000".into()),
        )]),
        pin: Some(norn_ir::data_quality::DataQualityRuleRef::Law {
            artifact_id: "demo.quantity.law".into(),
            law_id: "demo.nonnegative".into(),
            version: "a".repeat(64),
        }),
    });
    source
}
#[test]
fn explicit_grant_preserves_the_complete_shared_reason() {
    let scope = scope();
    let request = RefusalDetailRequest {
        scope: &scope,
        actor: "principal:operator",
        action_id: "inventory.cycle.receive",
    };
    let authorizer = PdpRefusalAuthorizer::new(
        DenyAll,
        pdp(&[DISCLOSE_REFUSAL_DETAIL_ACTION], true),
        claims,
    );
    assert!(authorizer.disclose_refusal_detail(&request));
    let source = refused();
    let expected = source.reason.clone();
    let actual = project_refusal(&authorizer, &request, source);
    assert_eq!(actual.code, "core.laws.violated");
    assert_eq!(actual.reason.as_deref(), expected.as_ref());
    assert_eq!(
        actual.explanation,
        "the proposed quantity violates the retained law"
    );
    for category in [
        norn_kernel::RefusalCategory::Visibility,
        norn_kernel::RefusalCategory::Internal,
    ] {
        let mut source = refused();
        source.category = category;
        let actual = project_refusal(&authorizer, &request, source);
        assert!(actual.reason.is_none());
    }
}
#[test]
fn absent_capability_policy_role_principal_or_scope_stays_redacted() {
    let scope = scope();
    let request = RefusalDetailRequest {
        scope: &scope,
        actor: "principal:operator",
        action_id: "inventory.cycle.receive",
    };
    assert!(!DenyAll.disclose_refusal_detail(&request));
    for (capabilities, declared) in [
        (&["norn.read"][..], true),
        (&[DISCLOSE_REFUSAL_DETAIL_ACTION][..], false),
    ] {
        let authorizer = PdpRefusalAuthorizer::new(DenyAll, pdp(capabilities, declared), claims);
        let actual = project_refusal(&authorizer, &request, refused());
        assert!(actual.reason.is_none());
        assert_eq!(actual.explanation, Refusal::UNPROVEN_DETAIL);
    }
    let authorizer = PdpRefusalAuthorizer::new(
        DenyAll,
        pdp(&[DISCLOSE_REFUSAL_DETAIL_ACTION], true),
        claims,
    );
    let mut wrong = scope.clone();
    wrong.environment_id = "prod".into();
    for (scope, actor, action_id) in [
        (&wrong, request.actor, request.action_id),
        (&scope, "unknown", request.action_id),
        (&scope, request.actor, "inventory.cycle.sell"),
    ] {
        assert!(!authorizer.disclose_refusal_detail(&RefusalDetailRequest {
            scope,
            actor,
            action_id
        }));
    }
    let viewer = PdpRefusalAuthorizer::new(
        DenyAll,
        pdp(&[DISCLOSE_REFUSAL_DETAIL_ACTION], true),
        |r: &RefusalDetailRequest<'_>| {
            let mut access = claims(r)?;
            access.principal.roles = vec!["Viewer".into()];
            Some(access)
        },
    );
    assert!(!viewer.disclose_refusal_detail(&request));
}
#[test]
fn trusted_mapper_cannot_change_the_bound_identity_action_resource_or_scope() {
    let scope = scope();
    let request = RefusalDetailRequest {
        scope: &scope,
        actor: "principal:operator",
        action_id: "inventory.cycle.receive",
    };
    for field in 0..7 {
        let authorizer = PdpRefusalAuthorizer::new(
            DenyAll,
            pdp(&[DISCLOSE_REFUSAL_DETAIL_ACTION], true),
            move |r: &RefusalDetailRequest<'_>| {
                let mut access = claims(r)?;
                match field {
                    0 => access.principal.subject_id = "other".into(),
                    1 => access.action_id = "norn.read".into(),
                    2 => access.resource.id = "inventory.cycle.transfer".into(),
                    3 => access.resource.scope.org = "foreign".into(),
                    4 => access.context.clear(),
                    5 => access.capability = "norn.read".into(),
                    _ => access.delegation = Delegation::Biscuit("unverified".into()),
                }
                Some(access)
            },
        );
        assert!(!authorizer.disclose_refusal_detail(&request));
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn generated_foreign_scopes_never_inherit_demo_detail(foreign in "[a-z_]{1,12}") {
        proptest::prop_assume!(foreign != "org_demo");
        let scope = CellScope { org_id: foreign, environment_id: "dev".into() };
        let request = RefusalDetailRequest { scope: &scope, actor: "principal:operator", action_id: "inventory.cycle.receive" };
        let authorizer = PdpRefusalAuthorizer::new(DenyAll, pdp(&[DISCLOSE_REFUSAL_DETAIL_ACTION], true), claims);
        proptest::prop_assert!(!authorizer.disclose_refusal_detail(&request));
    }
}
