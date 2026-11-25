//! HOST-READ-1 through the kernel PDP: Cedar decisions over the demo cell's
//! projected schema, reached through the shared `DevHost::read_access` gate.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "policy correspondence"
)]
use std::collections::BTreeMap;

use norn_api::authz::{ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell_do::policy::{
    Caller, CallerSlot, DEMO_GRANTS, READ_ACTION, ReadPolicy, capability_name, scope_key,
};
use norn_cell_do::principal::DEMO_FIXTURES;
use norn_cell_do::seed;
use norn_kernel::authz::{AccessRequest, ActorKind, Delegation, PrincipalInput, ResourceInput};
use std::sync::Arc;

const ALL: [ReadCapability; 3] = [
    ReadCapability::Population,
    ReadCapability::Values,
    ReadCapability::Sources,
];

/// The demo cell and the slot its policy reads the verified caller from.
struct Demo {
    host: DevHost<NativeEngine>,
    slot: Arc<CallerSlot>,
}

impl std::ops::Deref for Demo {
    type Target = DevHost<NativeEngine>;
    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

impl Demo {
    /// Record `subject` as the request's verified caller, with its fixture roles.
    fn as_caller(&self, subject: &str) {
        self.slot.record(caller(subject));
    }
}

fn caller(subject: &str) -> Option<Caller> {
    DEMO_FIXTURES
        .iter()
        .find(|(name, _)| *name == subject)
        .map(|(name, roles)| Caller {
            subject_id: (*name).to_owned(),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            actor_kind: "user",
            role_table: "fixtures/1",
            accountable_person: Some((*name).to_owned()),
        })
}

fn host() -> Demo {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let slot = policy.caller();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    Demo { host, slot }
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

fn gate(host: &Demo, access: &AccessRequest, capability: ReadCapability) -> Option<u16> {
    host.as_caller(&access.principal.subject_id);
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
            gate(&host, &access("admin@demo", capability, &scope), capability),
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
    let mut other = access("admin@demo", ReadCapability::Values, &scope);
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
    let request = access("admin@demo", ReadCapability::Values, &foreign);
    let denied = |access: &AccessRequest| {
        host.as_caller(&access.principal.subject_id);
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
    let mut undeclared = access("admin@demo", ReadCapability::Values, &scope);
    undeclared.resource.object_type = "canon.Undeclared".into();
    assert_eq!(denied(&undeclared).as_deref(), Some(baseline.as_str()));
    // A request naming a foreign cell in its explicit scope is denied too.
    host.as_caller("admin@demo");
    let mismatched = host.read_access(
        &explicit(&foreign),
        &ReadRequest {
            capability: ReadCapability::Values,
            access: &access("admin@demo", ReadCapability::Values, &scope),
        },
    );
    assert_eq!(
        serde_json::to_string(&mismatched.unwrap_err()).unwrap(),
        baseline
    );
}

#[test]
fn definition_reads_follow_the_grants_and_nothing_else() {
    let host = host();
    let query = norn_api::routes::DefinitionsQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        version: None,
    };
    let reads = |actor: Option<&str>| {
        host.as_caller(actor.unwrap_or_default());
        norn_api::routes::get_defs(&host, &query, actor).is_ok()
    };
    assert!(reads(Some("guest@demo")));
    assert!(!reads(Some("stranger@demo")));
    assert!(!reads(None));
}

/// Grants come only from the declared table: a verified caller whose roles are
/// all undeclared holds no capability, and a caller is never borrowed by
/// another subject.
#[test]
fn undeclared_roles_and_other_subjects_get_no_grant() {
    let host = host();
    let scope = host.scope().clone();
    for capability in ALL {
        host.slot.record(Some(Caller {
            subject_id: "user:u1".into(),
            roles: vec!["analyst".into(), "superuser".into()],
            actor_kind: "user",
            role_table: "fixtures/1",
            accountable_person: Some("user:u1".into()),
        }));
        let request = ReadRequest {
            capability,
            access: &access("user:u1", capability, &scope),
        };
        assert_eq!(
            host.read_access(&explicit(&scope), &request)
                .err()
                .map(|r| r.status),
            Some(403)
        );
        // The recorded caller is admin@demo; a request for another subject is not theirs.
        host.as_caller("admin@demo");
        let request = ReadRequest {
            capability,
            access: &access("guest@demo", capability, &scope),
        };
        assert_eq!(
            host.read_access(&explicit(&scope), &request)
                .err()
                .map(|r| r.status),
            Some(403)
        );
    }
    assert_eq!(DEMO_GRANTS.version, "norn-cell-do-grants/3");
    assert_eq!(DEMO_GRANTS.capabilities(&["analyst".into()]), []);
    assert_eq!(
        DEMO_GRANTS.capabilities(&["viewer".into()]),
        [ReadCapability::Population]
    );
}

/// The declared table, pinned exactly: a change of mapping is a new version.
#[test]
fn the_grant_table_is_pinned_exactly() {
    use ReadCapability::{Population, Sources, Values};
    let table: Vec<(&str, Vec<ReadCapability>)> = DEMO_GRANTS
        .entries
        .iter()
        .map(|(role, capabilities)| (*role, capabilities.to_vec()))
        .collect();
    assert_eq!(
        table,
        vec![
            ("viewer", vec![Population]),
            ("member", vec![Population, Values]),
            ("admin", vec![Population, Values, Sources]),
            ("owner", vec![Population, Values, Sources]),
        ]
    );
    assert_eq!(DEMO_GRANTS.version, "norn-cell-do-grants/3");
    let held = |roles: &[&str]| {
        DEMO_GRANTS.capabilities(&roles.iter().map(|r| (*r).to_owned()).collect::<Vec<_>>())
    };
    // Roles match exactly: no case folding, no trimming.
    for variant in ["Admin", "ADMIN", "admin ", " admin", "Viewer"] {
        assert_eq!(held(&[variant]), [], "{variant:?}");
    }
    // Duplicates and unions add nothing beyond the declared grants.
    assert_eq!(held(&["viewer", "viewer"]), [Population]);
    assert_eq!(held(&["viewer", "member"]), [Population, Values]);
    assert_eq!(held(&["viewer", "analyst"]), [Population]);
    // Only admins and owners write retained definitions.
    assert_eq!(DEMO_GRANTS.definition_writers, ["admin", "owner"]);
    // Only an owner provisions a cell.
    assert_eq!(DEMO_GRANTS.provisioners, ["owner"]);
    for (roles, provisions) in [
        (&["owner"][..], true),
        (&["admin"][..], false),
        (&["member"][..], false),
        (&["viewer"][..], false),
        (&["Owner"][..], false),
        (&[][..], false),
    ] {
        let roles: Vec<String> = roles.iter().map(|role| (*role).to_owned()).collect();
        assert_eq!(DEMO_GRANTS.provisions(&roles), provisions, "{roles:?}");
    }
    let writes = |roles: &[&str]| {
        DEMO_GRANTS.writes_definitions(&roles.iter().map(|r| (*r).to_owned()).collect::<Vec<_>>())
    };
    assert!(writes(&["admin"]) && writes(&["owner"]) && writes(&["viewer", "owner"]));
    for roles in [&["viewer"][..], &["member"], &["Admin"], &["analyst"], &[]] {
        assert!(!writes(roles), "{roles:?}");
    }
}

/// The grant is the caller's as a user; a request presenting the same subject
/// as a service is not that caller's grant.
#[test]
fn a_service_request_does_not_borrow_a_user_grant() {
    let host = host();
    let scope = host.scope().clone();
    let mut request = access("admin@demo", ReadCapability::Values, &scope);
    request.principal.actor_kind = ActorKind::Service;
    assert_eq!(gate(&host, &request, ReadCapability::Values), Some(403));
}

/// A verified caller ends with its request: afterwards a direct read naming
/// that subject is denied. Fails if the request guard is removed.
#[test]
fn a_request_s_caller_is_forgotten_when_the_request_ends() {
    use norn_cell_do::principal::FixturePrincipals;
    use norn_cell_do::server;
    let host = host();
    let scope = host.scope().clone();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, Arc::clone(&host.slot));
    for path in ["/v1/definitions", "/v1/not-a-route"] {
        let request = server::Request {
            method: "GET",
            path,
            query: "orgId=org_demo&environmentId=dev",
            authorization: Some("Bearer fixture:admin@demo"),
            now: 0,
            body: "",
        };
        let _ = server::handle(&host.host, &principals, &request);
        assert!(
            host.slot.current().is_none(),
            "{path}: the caller outlived its request"
        );
        let after = access("admin@demo", ReadCapability::Values, &scope);
        let denied = host.read_access(
            &explicit(&scope),
            &ReadRequest {
                capability: ReadCapability::Values,
                access: &after,
            },
        );
        assert_eq!(denied.err().map(|r| r.status), Some(403), "{path}");
    }
}

/// The policy keeps one PDP per grant (#1385); a kept PDP decides exactly as a
/// fresh one. Another subject, or the same subject with other roles, never
/// reuses it: each is decided by its own grant.
#[test]
fn a_kept_grant_is_never_another_caller_s() {
    let host = host();
    let scope = host.scope().clone();
    let values = ReadCapability::Values;
    for _round in 0..3 {
        assert_eq!(
            gate(&host, &access("admin@demo", values, &scope), values),
            None
        );
        assert_eq!(
            gate(&host, &access("guest@demo", values, &scope), values),
            Some(403)
        );
        // admin@demo's subject, verified this time with only the viewer role.
        host.slot.record(Some(Caller {
            subject_id: "admin@demo".into(),
            roles: vec!["viewer".into()],
            actor_kind: "user",
            role_table: "fixtures/1",
            accountable_person: Some("admin@demo".into()),
        }));
        let request = ReadRequest {
            capability: values,
            access: &access("admin@demo", values, &scope),
        };
        assert_eq!(
            host.read_access(&explicit(&scope), &request)
                .err()
                .map(|refusal| refusal.status),
            Some(403)
        );
        // A request naming the kept subject while another caller is verified.
        host.as_caller("guest@demo");
        let request = ReadRequest {
            capability: values,
            access: &access("admin@demo", values, &scope),
        };
        assert_eq!(
            host.read_access(&explicit(&scope), &request)
                .err()
                .map(|refusal| refusal.status),
            Some(403)
        );
    }
}

/// An install that changes the model replaces the kept PDPs with the new
/// rules (#1385): a type added is readable at once, and a type removed is
/// denied even to a caller whose PDP was kept before the change.
#[test]
fn a_model_change_replaces_every_kept_grant() {
    use norn_api::authz::Authorizer;
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let types: Vec<_> = store.model().object_types().cloned().collect();
    let policy = ReadPolicy::compile(&types, &scope, &DEMO_GRANTS).unwrap();
    policy.caller().record(caller("admin@demo"));
    let reads = |object_type: &str| {
        let mut request = access("admin@demo", ReadCapability::Values, &scope);
        object_type.clone_into(&mut request.resource.object_type);
        object_type.clone_into(&mut request.resource.id);
        policy
            .read(&ReadRequest {
                capability: ReadCapability::Values,
                access: &request,
            })
            .is_ok()
    };
    let mut added = types
        .iter()
        .find(|object_type| object_type.name.as_str() == "canon.Item")
        .cloned()
        .unwrap();
    added.name = norn_ir::ObjectTypeName("canon.Added".into());
    assert!(reads("canon.Item"));
    assert!(!reads("canon.Added"));
    let mut grown = types.clone();
    grown.push(added);
    policy.model_changed(&grown);
    assert!(reads("canon.Added"));
    assert!(reads("canon.Item"));
    // The PDP kept under the grown rules allows canon.Added; it must not
    // outlive them.
    policy.model_changed(&types);
    assert!(!reads("canon.Added"));
    assert!(reads("canon.Item"));
}
