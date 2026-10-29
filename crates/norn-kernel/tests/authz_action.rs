//! PDP action-adapter contract tests. These build a real Cedar PDP and keep
//! caller identity and visibility decisions in the trusted test shell mapper.
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeMap;

use norn_authz::access::AccessIr;
use norn_authz::{
    AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, DenyReason, Pdp,
    PrincipalInput, ResourceInput, ScopeKey, compile,
};
use norn_ir::ActionRoot;
use norn_kernel::authz::PdpActionAuthorizer;
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{ActionAuthorizer, ActionAuthzRequest, ActionDenial, FactKey};

const PROJECTED: &str = r#"
namespace app { entity Doc = { scope: Norn::Scope }; }
namespace Norn {
  entity Actor in [Role] = { scope: Scope };
  action "doc.approve" appliesTo {
    principal: [Actor],
    resource: [app::Doc],
    context: { scope: Scope }
  };
}
"#;

const ACCESS: &str = r#"{
  "accessVersion": "0.1",
  "policyId": "app.docs",
  "rules": [{
    "ruleId": "approve_in_scope",
    "actions": ["doc.approve"],
    "resourceType": "app.Doc",
    "scope": { "dimensions": ["org"] }
  }]
}"#;

fn scope() -> ScopeKey {
    ScopeKey {
        org: "org-demo".to_owned(),
        tenant: Some("tenant-a".to_owned()),
        workspace: None,
        environment: Some("dev".to_owned()),
        business_unit: None,
    }
}

fn pdp(granted_capabilities: &[&str]) -> Pdp {
    let schema = AuthzSchema::from_projected_cedarschema(PROJECTED).expect("projected schema");
    let access: AccessIr = serde_json::from_str(ACCESS).expect("AccessIR");
    let access = compile(&schema, &access).expect("admitted policy");
    let grants = [CapabilityGrant {
        subject_id: "alice".to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(scope()),
        capabilities: granted_capabilities
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        binding: None,
    }];
    Pdp::new(access, &grants).expect("compiled grants")
}

fn action() -> ActionRoot {
    serde_json::from_str(
        r#"{
          "actionId": "doc.approve",
          "scopeRequired": ["orgId"],
          "idempotent": false,
          "ledgered": false,
          "inputSchema": {"type": "object"},
          "outputSchema": {"type": "object"}
        }"#,
    )
    .expect("ActionIR")
}

fn kernel_scope() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org-demo".to_owned()),
        ("tenantId".to_owned(), "tenant-a".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

fn action_request<'a>(
    action: &'a ActionRoot,
    scope: &'a BTreeMap<String, String>,
    input: &'a JsonObject,
    target: &'a FactKey,
    target_values: Option<&'a JsonObject>,
) -> ActionAuthzRequest<'a> {
    ActionAuthzRequest {
        action,
        scope,
        actor: "alice",
        input,
        target,
        target_values,
    }
}

fn mapped_request(request: &ActionAuthzRequest<'_>, capability: &str) -> AccessRequest {
    let request_scope = ScopeKey {
        org: request.scope.get("orgId").expect("org scope").clone(),
        tenant: request.scope.get("tenantId").cloned(),
        workspace: request.scope.get("workspaceId").cloned(),
        environment: request.scope.get("environmentId").cloned(),
        business_unit: request.scope.get("businessUnitId").cloned(),
    };
    AccessRequest {
        principal: PrincipalInput {
            subject_id: request.actor.to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: Vec::new(),
            attributes: BTreeMap::new(),
        },
        scope: Some(request_scope.clone()),
        capability: capability.to_owned(),
        action_id: request.action.action_id.clone(),
        resource: ResourceInput {
            object_type: request.target.object_type.as_str().to_owned(),
            id: request.target.key.clone(),
            scope: request_scope,
            attributes: BTreeMap::new(),
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}

struct CallFixture {
    action: ActionRoot,
    scope: BTreeMap<String, String>,
    input: JsonObject,
    target: FactKey,
    values: JsonObject,
}

fn call() -> CallFixture {
    CallFixture {
        action: action(),
        scope: kernel_scope(),
        input: JsonObject::from([("note".to_owned(), JsonValue::String("hello".to_owned()))]),
        target: FactKey::new("app.Doc", "doc-1"),
        values: JsonObject::from([("title".to_owned(), JsonValue::String("Invoice".to_owned()))]),
    }
}

#[test]
fn action_authorizer_maps_real_pdp_allow_and_deny() {
    let call = call();
    let request = action_request(
        &call.action,
        &call.scope,
        &call.input,
        &call.target,
        Some(&call.values),
    );
    let allow = PdpActionAuthorizer::new(pdp(&["doc.approve"]), |request| {
        Ok(mapped_request(request, "doc.approve"))
    });
    assert_eq!(allow.authorize(&request), Ok(()));

    let deny = PdpActionAuthorizer::new(pdp(&["doc.read"]), |request| {
        Ok(mapped_request(request, "doc.approve"))
    });
    assert_eq!(
        deny.authorize(&request),
        Err(ActionDenial::Forbidden {
            reason: DenyReason::Capability.code().to_owned(),
        })
    );
}

#[test]
fn action_authorizer_preserves_shell_visibility_denial() {
    let call = call();
    let request = action_request(
        &call.action,
        &call.scope,
        &call.input,
        &call.target,
        Some(&call.values),
    );
    let authorizer =
        PdpActionAuthorizer::new(pdp(&["doc.approve"]), |_| Err(ActionDenial::Invisible));
    assert_eq!(authorizer.authorize(&request), Err(ActionDenial::Invisible));
}

#[test]
fn action_authorizer_rejects_mapper_target_and_scope_mismatches() {
    let call = call();
    let request = action_request(
        &call.action,
        &call.scope,
        &call.input,
        &call.target,
        Some(&call.values),
    );

    let mismatches: [fn(&mut AccessRequest); 4] = [
        |mapped| mapped.principal.subject_id = "mallory".to_owned(),
        |mapped| mapped.action_id = "doc.read".to_owned(),
        |mapped| mapped.resource.object_type = "app.Other".to_owned(),
        |mapped| mapped.resource.id = "other-doc".to_owned(),
    ];
    for change in mismatches {
        let authorizer = PdpActionAuthorizer::new(pdp(&["doc.approve"]), move |request| {
            let mut mapped = mapped_request(request, "doc.approve");
            change(&mut mapped);
            Ok(mapped)
        });
        assert_eq!(
            authorizer.authorize(&request),
            Err(ActionDenial::Forbidden {
                reason: DenyReason::InvalidRequest.code().to_owned(),
            })
        );
    }

    let wrong_scope = PdpActionAuthorizer::new(pdp(&["doc.approve"]), |request| {
        let mut mapped = mapped_request(request, "doc.approve");
        mapped.scope.as_mut().expect("scope").org = "other-org".to_owned();
        Ok(mapped)
    });
    assert_eq!(
        wrong_scope.authorize(&request),
        Err(ActionDenial::Forbidden {
            reason: DenyReason::InvalidRequest.code().to_owned(),
        })
    );

    let mut unknown_dimension = call.scope.clone();
    unknown_dimension.insert("divisionId".to_owned(), "x".to_owned());
    let request = action_request(
        &call.action,
        &unknown_dimension,
        &call.input,
        &call.target,
        Some(&call.values),
    );
    let unsupported_scope = PdpActionAuthorizer::new(pdp(&["doc.approve"]), |request| {
        Ok(mapped_request(request, "doc.approve"))
    });
    assert_eq!(
        unsupported_scope.authorize(&request),
        Err(ActionDenial::Forbidden {
            reason: DenyReason::InvalidRequest.code().to_owned(),
        })
    );
}
