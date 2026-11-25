//! PRINCIPAL-INPUT-1 (#1091): this host's resolver, through the cell's own
//! compiled policies: the verified caller, the scope, the actor kinds this
//! host issues, a missing policy, and a recorded principal acting.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_api::authz::Authorizer;
use norn_api::scope::CellScope;
use norn_ir::rule::PrincipalInputDeclaration;
use norn_ir::{ActionRoot, ArtifactEnvelope, RuleRoot, RuntimeValue};
use norn_kernel::{FactKey, PrincipalInputsRequest};
use serde_json::{Value, json};

use super::{decode, resolve};
use crate::actions::ActionPolicy;
use crate::policy::{Caller, DEMO_GRANTS, ReadPolicy};

const DOCUMENT: &str = "review.Document";
const APPROVE: &str = "review.document.approve";

fn envelope(kind: &str, artifact_id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": artifact_id, "kind": kind, "namespace": "review",
        "name": artifact_id, "version": "1.0.0", "lifecycle": "active",
        "scope": {}, "dependencies": [],
        "provenance": {"source": "system", "sourcePath": "test"},
        "payload": payload
    }))
    .expect("artifact")
}

fn empty_list() -> Value {
    json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}})
}

/// A document type, its access policy with a principal catalog, an approval
/// decision that declares three principal inputs, and the action pinning it.
fn artifacts() -> Vec<ArtifactEnvelope> {
    let entity = json!({"entityVersion": "2.0", "objectType": {
        "name": DOCUMENT,
        "properties": [
            {"id": "id", "type": {"kind": "text"}},
            {"id": "confirmed", "type": {"kind": "boolean"}, "default": false},
            {"id": "status", "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "approved"}]}, "default": "draft"}
        ],
        "lifecycles": [{"property": "status", "initial": ["draft"], "terminal": ["approved"],
            "transitions": [{"id": "approve", "from": ["draft"], "to": "approved"}]}]
    }});
    let policy = json!({"accessVersion": "0.1", "policyId": "review.access",
    "rules": [{"ruleId": "approvers", "actions": [APPROVE], "resourceType": DOCUMENT,
        "scope": {"dimensions": ["org", "environment"]},
        "allOf": [{"role": {"anyOf": ["member", "admin", "owner"]}}]}],
    "principalCatalog": [
        {"name": "admin", "type": "bool", "source": {"kind": "role"}},
        {"name": "staff", "type": "stringSet", "source": {"kind": "roleProfile", "roles": ["admin", "owner"]}},
        {"name": "id", "type": "string", "source": {"kind": "attribute"}}
    ]});
    let rule = json!({
        "ruleVersion": "2.0", "ruleId": "review.approval.decide", "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "principalInputs": [
            {"name": "isAdmin", "kind": {"holdsRole": {"role": "admin", "at": "resource"}}},
            {"name": "staffRoles", "kind": {"roleProfile": {"profile": "staff"}}},
            {"name": "actorId", "kind": {"attribute": {"name": "id"}}}
        ],
        "inputSchema": {"type": "object", "additionalProperties": false,
            "required": ["slice", "event", "principal"],
            "properties": {
                "slice": {"type": "object", "additionalProperties": false, "properties": {}},
                "event": {"type": "object", "additionalProperties": false, "properties": {}},
                "principal": {"type": "object", "additionalProperties": false,
                    "required": ["isAdmin", "staffRoles", "actorId"],
                    "properties": {
                        "isAdmin": {"type": "boolean"},
                        "staffRoles": {"type": "array", "items": {"type": "string"}},
                        "actorId": {"type": "string"}
                    }}
            }},
        "outputSchema": {"type": "object", "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": {"facts": empty_list(), "commands": empty_list(), "timers": empty_list()}},
        "graph": {"nodes": [], "edges": []}
    });
    let record = json!({"type": "object", "additionalProperties": false,
        "required": ["id"], "properties": {"id": {"type": "string"}}});
    let action = json!({
        "actionId": APPROVE, "title": "Approve", "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true, "inputSchema": record, "outputSchema": record,
        "access": {"artifactId": "review.access.policy", "version": "1.0.0"},
        "synchronousRule": {"artifactId": "review.approval.decide.rule", "version": "1.0.0"},
        "writes": [{"entityKind": DOCUMENT, "change": "modified"}],
        "effectProgram": {"kind": "fire_transition", "entityArtifactId": "review.Document.entity",
            "entityArtifactVersion": "1.0.0", "objectKind": DOCUMENT, "objectIdField": "id",
            "lifecycle": "status", "transition": "approve"}
    });
    vec![
        envelope("entity", "review.Document.entity", &entity),
        envelope("policy", "review.access.policy", &policy),
        envelope("rule", "review.approval.decide.rule", &rule),
        envelope("action", "review.document.approve.action", &action),
    ]
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "prod".into(),
    }
}

fn caller(subject: &str, role: &str, actor_kind: &'static str) -> Caller {
    Caller {
        subject_id: subject.to_owned(),
        roles: vec![role.to_owned()],
        actor_kind,
        role_table: "studio-roles/1",
    }
}

/// The compiled action policy, the action and the decision's declared inputs.
struct Fixture {
    policy: ActionPolicy,
    read: ReadPolicy,
    action: ActionRoot,
    inputs: Vec<PrincipalInputDeclaration>,
    scope: BTreeMap<String, String>,
    subject: FactKey,
}

impl Fixture {
    fn new() -> Self {
        let artifacts = artifacts();
        let model = norn_api::kernel_set::kernel_model(&artifacts).expect("model");
        let compile = || {
            ActionPolicy::compile(&model, &artifacts, &scope())
                .expect("the policy compiles")
                .expect("the action is callable")
        };
        let read =
            ReadPolicy::compile(model.object_types(), &scope(), &DEMO_GRANTS).expect("read policy");
        *read.action_slot().write().expect("slot") = Some(Arc::new(compile()));
        let rule: RuleRoot = decode(&artifacts[2]).expect("rule");
        Self {
            policy: compile(),
            read,
            action: decode(&artifacts[3]).expect("action"),
            inputs: rule.principal_inputs,
            scope: BTreeMap::from([
                ("orgId".to_owned(), "org_a".to_owned()),
                ("environmentId".to_owned(), "prod".to_owned()),
            ]),
            subject: FactKey::new(DOCUMENT, "DOC-1"),
        }
    }

    fn request<'a>(&'a self, actor: &'a str) -> PrincipalInputsRequest<'a> {
        PrincipalInputsRequest {
            action: &self.action,
            scope: &self.scope,
            actor,
            subject: &self.subject,
            inputs: &self.inputs,
        }
    }
}

fn text(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

#[test]
fn the_verified_caller_resolves_its_own_values_under_a_named_policy_version() {
    let fixture = Fixture::new();
    let host = fixture.policy.principal_resolver();
    let admin = caller("user:ada", "admin", "user");
    let resolved = host
        .resolve(&admin, DEMO_GRANTS.version, &fixture.request("user:ada"))
        .expect("resolved");
    assert_eq!(resolved.principal, "user:ada");
    assert_eq!(resolved.subject, fixture.subject);
    assert_eq!(
        resolved.values,
        BTreeMap::from([
            ("actorId".to_owned(), text("user:ada")),
            ("isAdmin".to_owned(), RuntimeValue::Bool(true)),
            (
                "staffRoles".to_owned(),
                RuntimeValue::Array(vec![text("admin")])
            ),
        ])
    );
    // The policy version names the role table, the grant table and the
    // catalog, and is the same for the same definitions.
    let prefix = "roles=studio-roles/1;grants=norn-cell-do-grants/3;catalog=";
    let catalog = resolved.policy_version.strip_prefix(prefix).expect("shape");
    assert!(catalog.len() >= 32, "{catalog}");
    // A member holds neither the role nor any of the profile's.
    let member = caller("user:mo", "member", "user");
    let resolved_member = host
        .resolve(&member, DEMO_GRANTS.version, &fixture.request("user:mo"))
        .expect("resolved");
    assert_eq!(resolved_member.values["isAdmin"], RuntimeValue::Bool(false));
    assert_eq!(
        resolved_member.values["staffRoles"],
        RuntimeValue::Array(vec![])
    );
    assert_eq!(resolved_member.policy_version, resolved.policy_version);
}

#[test]
fn another_actor_another_scope_or_an_unissued_actor_kind_resolves_nothing() {
    let fixture = Fixture::new();
    let host = fixture.policy.principal_resolver();
    let admin = caller("user:ada", "admin", "user");
    // The request names another actor than the verified caller.
    let other = host.resolve(&admin, DEMO_GRANTS.version, &fixture.request("user:eve"));
    assert_eq!(other.expect_err("another actor").input, "isAdmin");
    // The request is for another scope than the cell's.
    let mut foreign = fixture.request("user:ada");
    let elsewhere = BTreeMap::from([
        ("orgId".to_owned(), "org_a".to_owned()),
        ("environmentId".to_owned(), "staging".to_owned()),
    ]);
    foreign.scope = &elsewhere;
    assert!(host.resolve(&admin, DEMO_GRANTS.version, &foreign).is_err());
    let no_scope = BTreeMap::new();
    foreign.scope = &no_scope;
    assert!(host.resolve(&admin, DEMO_GRANTS.version, &foreign).is_err());
    // This host issues users and services; an agent kind is not acted for.
    let agent = caller("user:ada", "admin", "agent");
    assert!(
        host.resolve(&agent, DEMO_GRANTS.version, &fixture.request("user:ada"))
            .is_err()
    );
}

#[test]
fn without_a_verified_member_or_an_installed_policy_nothing_resolves() {
    let fixture = Fixture::new();
    let admin = caller("user:ada", "admin", "user");
    let request = fixture.request("user:ada");
    assert!(resolve(Some(&admin), Some(&fixture.policy), &DEMO_GRANTS, &request).is_ok());
    assert!(resolve(None, Some(&fixture.policy), &DEMO_GRANTS, &request).is_err());
    assert!(resolve(Some(&admin), None, &DEMO_GRANTS, &request).is_err());
}

#[test]
fn the_cells_policy_resolves_for_the_requests_caller_and_never_for_an_acting_one() {
    let fixture = Fixture::new();
    let slot = fixture.read.caller();
    let request = fixture.request("user:ada");
    // Nobody verified: nothing resolves.
    assert!(fixture.read.principal_inputs(&request).is_err());
    // The request's own verified caller resolves.
    slot.record(Some(caller("user:ada", "admin", "user")));
    let resolved = fixture.read.principal_inputs(&request).expect("resolved");
    assert_eq!(resolved.values["isAdmin"], RuntimeValue::Bool(true));
    // The same principal recorded earlier and acting for a run or an
    // approved call carries roles of another request: it fails closed.
    let previous = slot.replace(Some(caller("user:ada", "admin", "user")), true);
    let refused = fixture.read.principal_inputs(&request).expect_err("acting");
    assert_eq!(refused.input, "isAdmin");
    // Once the request's own caller is restored, it resolves again.
    slot.replace(previous.0, previous.1);
    assert!(fixture.read.principal_inputs(&request).is_ok());
    // A role the grant table does not declare is not a member of the cell.
    slot.record(Some(caller("user:ada", "auditor", "user")));
    assert!(fixture.read.principal_inputs(&request).is_err());
}
