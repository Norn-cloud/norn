//! HOST-READ-1 through the kernel PDP: Cedar decisions over the demo cell's
//! projected schema, reached through the shared `DevHost::read_access` gate.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "policy correspondence"
)]
use std::collections::BTreeMap;

use norn_api::authz::{Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell_do::policy::{DEMO_GRANTS, READ_ACTION, ReadPolicy, capability_name, scope_key};
use norn_cell_do::seed;
use norn_kernel::authz::{AccessRequest, ActorKind, Delegation, PrincipalInput, ResourceInput};

const ALL: [ReadCapability; 3] = [
    ReadCapability::Population,
    ReadCapability::Values,
    ReadCapability::Sources,
];

fn host() -> DevHost<NativeEngine> {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, DEMO_GRANTS).unwrap();
    DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy)
}

/// What the trusted server mapping builds for one principal, capability and row.
fn access(subject: &str, capability: ReadCapability, scope: &CellScope) -> AccessRequest {
    let key = scope_key(scope);
    AccessRequest {
        principal: PrincipalInput {
            subject_id: subject.into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![],
            attributes: BTreeMap::new(),
        },
        scope: Some(key.clone()),
        capability: capability_name(capability).into(),
        action_id: READ_ACTION.into(),
        resource: ResourceInput {
            object_type: "canon.Item".into(),
            id: "demo/item/gold".into(),
            scope: key,
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}

fn explicit(scope: &CellScope) -> ScopeQuery {
    ScopeQuery {
        org_id: Some(scope.org_id.clone()),
        environment_id: Some(scope.environment_id.clone()),
    }
}

fn gate(
    host: &DevHost<NativeEngine>,
    access: &AccessRequest,
    capability: ReadCapability,
) -> Option<u16> {
    let request = ReadRequest { capability, access };
    host.read_access(&explicit(host.scope()), &request)
        .err()
        .map(|refusal| refusal.status)
}

#[test]
fn each_capability_is_granted_only_where_the_policy_grants_it() {
    let host = host();
    let scope = host.scope().clone();
    for capability in ALL {
        assert_eq!(
            gate(
                &host,
                &access("viewer@demo", capability, &scope),
                capability
            ),
            None
        );
        let guest = gate(&host, &access("guest@demo", capability, &scope), capability);
        assert_eq!(
            guest,
            (capability != ReadCapability::Population).then_some(403),
            "{capability:?}"
        );
        assert_eq!(
            gate(
                &host,
                &access("stranger@demo", capability, &scope),
                capability
            ),
            Some(403)
        );
    }
}

#[test]
fn a_mapping_that_asks_for_another_capability_or_action_is_denied() {
    let host = host();
    let scope = host.scope().clone();
    // Population's grant name never stands in for Values or Sources.
    let population = access("guest@demo", ReadCapability::Population, &scope);
    assert_eq!(gate(&host, &population, ReadCapability::Values), Some(403));
    assert_eq!(gate(&host, &population, ReadCapability::Sources), Some(403));
    let mut other = access("viewer@demo", ReadCapability::Values, &scope);
    other.action_id = "norn.reveal".into();
    assert_eq!(gate(&host, &other, ReadCapability::Values), Some(403));
}

#[test]
fn foreign_scope_and_undeclared_types_are_denied_with_one_document() {
    let host = host();
    let scope = host.scope().clone();
    let foreign = CellScope {
        org_id: "org_other".into(),
        environment_id: "dev".into(),
    };
    let request = access("viewer@demo", ReadCapability::Values, &foreign);
    let denied = |access: &AccessRequest| {
        host.read_access(
            &explicit(&scope),
            &ReadRequest {
                capability: ReadCapability::Values,
                access,
            },
        )
        .err()
        .map(|refusal| serde_json::to_string(&refusal).unwrap())
    };
    let baseline = denied(&access("stranger@demo", ReadCapability::Values, &scope)).unwrap();
    assert_eq!(denied(&request).as_deref(), Some(baseline.as_str()));
    let mut undeclared = access("viewer@demo", ReadCapability::Values, &scope);
    undeclared.resource.object_type = "canon.Undeclared".into();
    assert_eq!(denied(&undeclared).as_deref(), Some(baseline.as_str()));
    // A request naming a foreign cell in its explicit scope is denied too.
    let mismatched = host.read_access(
        &explicit(&foreign),
        &ReadRequest {
            capability: ReadCapability::Values,
            access: &access("viewer@demo", ReadCapability::Values, &scope),
        },
    );
    assert_eq!(
        serde_json::to_string(&mismatched.unwrap_err()).unwrap(),
        baseline
    );
}

#[test]
fn definition_reads_follow_the_grants_and_nothing_else() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, DEMO_GRANTS).unwrap();
    let request = |actor, operation| norn_api::authz::AccessRequest {
        scope: &scope,
        actor,
        operation,
    };
    use norn_api::authz::Operation;
    assert!(
        policy
            .authorize(&request(Some("guest@demo"), Operation::ReadDefinitions))
            .is_ok()
    );
    assert!(
        policy
            .authorize(&request(Some("stranger@demo"), Operation::ReadDefinitions))
            .is_err()
    );
    assert!(
        policy
            .authorize(&request(None, Operation::ReadDefinitions))
            .is_err()
    );
    assert!(
        policy
            .authorize(&request(
                Some("viewer@demo"),
                Operation::CallAction("any".into())
            ))
            .is_err()
    );
}
