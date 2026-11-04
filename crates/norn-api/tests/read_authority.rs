//! HOST-READ-1: default denial, joint gates and physical-scope correspondence.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::expect_used, reason = "read authority conformance fixtures")]
use norn_api::{
    authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest},
    host::DevHost,
    refusal::Refusal,
    scope::{CellScope, ScopeQuery},
};
use norn_cell::{CellStore, Model};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn host(scope: CellScope) -> DevHost<norn_cell::engine::NativeEngine> {
    let model = Model::from_definitions(&[], &[]).expect("empty model");
    DevHost::from_store(
        CellStore::open_in_memory(model).expect("store"),
        scope,
        |_| BTreeMap::new(),
    )
    .expect("host")
}
fn scope() -> CellScope {
    CellScope {
        org_id: "example".into(),
        environment_id: "dev".into(),
    }
}
fn key(scope: &CellScope) -> ScopeKey {
    ScopeKey {
        org: scope.org_id.clone(),
        environment: Some(scope.environment_id.clone()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}
fn request(scope: &CellScope) -> PdpRequest {
    PdpRequest {
        principal: PrincipalInput {
            subject_id: "viewer".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![],
            attributes: BTreeMap::new(),
        },
        scope: Some(key(scope)),
        capability: "example.read".into(),
        action_id: "norn.read".into(),
        resource: ResourceInput {
            object_type: "example.Record".into(),
            id: "all".into(),
            scope: key(scope),
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}
struct Metadata;
impl Authorizer for Metadata {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
}
struct Policy {
    calls: Arc<AtomicUsize>,
    physical: ScopeKey,
    allowed: bool,
}
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, read: &ReadRequest<'_>) -> Result<(), Refusal> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(read.access.scope.as_ref(), Some(&self.physical));
        assert_eq!(read.access.resource.scope, self.physical);
        if self.allowed && read.capability == ReadCapability::Population {
            Ok(())
        } else {
            Err(Refusal::invalid("private policy detail"))
        }
    }
}
fn explicit(scope: &CellScope) -> ScopeQuery {
    ScopeQuery {
        org_id: Some(scope.org_id.clone()),
        environment_id: Some(scope.environment_id.clone()),
    }
}

#[test]
fn metadata_grants_do_not_grant_read_capabilities() {
    let physical = scope();
    let access = request(&physical);
    for host in [
        host(physical.clone()),
        host(physical.clone()).with_authorizer(Metadata),
    ] {
        for capability in [
            ReadCapability::Population,
            ReadCapability::Values,
            ReadCapability::Sources,
        ] {
            let denied = host
                .read_access(
                    &explicit(&physical),
                    &ReadRequest {
                        capability,
                        access: &access,
                    },
                )
                .expect_err("explicit capability required");
            assert_eq!((denied.status, denied.code.as_str()), (403, "authz.denied"));
        }
    }
}

#[test]
fn read_gate_checks_policy_and_scope_before_reading() {
    let physical = scope();
    let access = request(&physical);
    let baseline = host(physical.clone())
        .read_access(
            &explicit(&physical),
            &ReadRequest {
                capability: ReadCapability::Population,
                access: &access,
            },
        )
        .expect_err("baseline");
    let bytes = serde_json::to_vec(&baseline).expect("wire");
    let queries = [
        explicit(&physical),
        ScopeQuery::default(),
        ScopeQuery {
            org_id: Some("foreign".into()),
            environment_id: Some("dev".into()),
        },
    ];
    for allowed in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let host = host(physical.clone()).with_authorizer(Policy {
            calls: Arc::clone(&calls),
            physical: key(&physical),
            allowed,
        });
        for (i, query) in queries.iter().enumerate() {
            let result = host.read_access(
                query,
                &ReadRequest {
                    capability: ReadCapability::Population,
                    access: &access,
                },
            );
            if allowed && i == 0 {
                assert!(result.is_ok());
            } else {
                let denied = result.expect_err("joint read gate");
                assert_eq!(denied.status, 403);
                assert_eq!(serde_json::to_vec(&denied).expect("wire"), bytes);
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), queries.len());
        for capability in [ReadCapability::Values, ReadCapability::Sources] {
            assert!(
                host.read_access(
                    &explicit(&physical),
                    &ReadRequest {
                        capability,
                        access: &access
                    }
                )
                .is_err()
            );
        }
    }
}

#[test]
fn physical_scope_is_the_only_policy_scope() {
    let physical = scope();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = host(physical.clone()).with_authorizer(Policy {
        calls: Arc::clone(&calls),
        physical: key(&physical),
        allowed: true,
    });
    let mut access = request(&physical);
    access.scope = None;
    assert!(
        host.read_access(
            &explicit(&physical),
            &ReadRequest {
                capability: ReadCapability::Population,
                access: &access
            }
        )
        .is_err()
    );
    access.scope = Some(key(&physical));
    access.resource.scope.org = "foreign".into();
    assert!(
        host.read_access(
            &explicit(&physical),
            &ReadRequest {
                capability: ReadCapability::Population,
                access: &access
            }
        )
        .is_err()
    );
    access.resource.scope = key(&physical);
    access.resource.scope.tenant = Some("unbound".into());
    assert!(
        host.read_access(
            &explicit(&physical),
            &ReadRequest {
                capability: ReadCapability::Population,
                access: &access
            }
        )
        .is_err()
    );
    access.resource.scope = key(&physical);
    access.action_id = "example.write".into();
    assert!(
        host.read_access(
            &explicit(&physical),
            &ReadRequest {
                capability: ReadCapability::Population,
                access: &access
            }
        )
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn denied_read_documents_do_not_reveal_the_physical_scope(org in "[a-z]{1,16}", env in "[a-z]{1,16}") {
        let physical = CellScope { org_id: org, environment_id: env };
        let access = request(&physical);
        let baseline = host(scope()).read_access(&ScopeQuery::default(), &ReadRequest { capability: ReadCapability::Population, access: &access }).expect_err("deny");
        let denied = host(physical.clone()).read_access(&explicit(&physical), &ReadRequest { capability: ReadCapability::Population, access: &access }).expect_err("deny");
        proptest::prop_assert_eq!(denied.status, baseline.status);
        proptest::prop_assert_eq!(serde_json::to_vec(&denied).expect("wire"), serde_json::to_vec(&baseline).expect("wire"));
    }
}
