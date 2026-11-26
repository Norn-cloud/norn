//! DEV-PRINCIPALS-1: a named local principal holds exactly its declared
//! roles. It reads, and the active policy decides each call it makes; it
//! decides only a decision one of its roles may decide; and it holds no
//! hosted-admin operation unless the hosted grant table gives its roles
//! that. The developer and the reviewer keep every grant of the loopback
//! cell. Driven through `norn dev`'s own save path and dispatch.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use norn_api::authz::{AccessRequest, Authorizer, Operation};
use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use serde_json::{Value, json};

use crate::dev::http::{self, Serving};
use crate::dev::policy::{
    self, DevMetrics, DevPrincipals, LocalDeveloper, NamedPrincipals, reviewer,
};
use crate::dev::{Events, Manifest, Session};
use crate::principal::PrincipalResolver as _;
use crate::server::Request;

use crate::dev::admit::principal_inputs_tests::review_pack::{
    self, APPROVE, admin_role, approve_action, review_base,
};

const ACTOR: &str = "local:developer";
const RECORD: &str = "review.document.record";
/// Holds the role that records a document, and nothing else.
const CLERK: &str = "clerk";
/// Holds the role that approves one.
const BUYER: &str = "buyer";
/// Holds the hosted `admin` role, which writes definitions and decides the
/// pack's approval.
const OPS: &str = "ops";
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

fn scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

fn token(actor: &str) -> String {
    format!("{actor}-token-0123456789abcdef")
}

fn named() -> NamedPrincipals {
    Arc::new(BTreeMap::from([
        (CLERK.to_owned(), vec!["warehouse_clerk".to_owned()]),
        (BUYER.to_owned(), vec!["buyer".to_owned()]),
        (OPS.to_owned(), vec!["admin".to_owned()]),
    ]))
}

/// The review pack, with its own roles: a clerk records, a buyer approves.
fn pack() -> Vec<Value> {
    let rule = |id: &str, action: &str, role: &str| {
        json!({ "ruleId": id, "actions": [action], "resourceType": review_pack::DOCUMENT,
            "scope": { "dimensions": ["org", "environment"] },
            "allOf": [{ "role": { "anyOf": [role] } }] })
    };
    let mut pack = review_base(&admin_role());
    let policy = pack
        .iter_mut()
        .find(|artifact| artifact["kind"] == "policy")
        .expect("the access policy");
    policy["payload"]["rules"] = json!([
        rule("clerks", RECORD, "warehouse_clerk"),
        rule("buyers", APPROVE, "buyer"),
    ]);
    pack.push(approve_action("0.1.0", None));
    pack
}

/// The pack's approval: an approve waits for an `admin` to decide it.
fn gate() -> Value {
    json!({ "kind": "approval", "id": "review.approve_gate", "payload": {
        "action": APPROVE, "decider": { "roles": ["admin"] },
        "question": "Approve this document?", "risk": "high" } })
}

struct Lab {
    _directory: tempfile::TempDir,
    serving: Serving,
    principals: DevPrincipals,
    metrics: DevMetrics,
}

/// A `norn dev` session over the pack and its approval, with the named
/// principals, saved through [`Session::compiled`].
fn lab() -> Lab {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let serving: Serving = Arc::new(RwLock::new(None));
    let manifest: Manifest = serde_json::from_value(json!({
        "format": "norn-project/1", "name": "review", "sources": [],
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dev": { "port": 0, "actor": ACTOR, "principals": [
            { "actor": CLERK, "roles": ["warehouse_clerk"] },
            { "actor": BUYER, "roles": ["buyer"] },
            { "actor": OPS, "roles": ["admin"] }
        ] }, "cell": "cell.sqlite"
    }))
    .expect("manifest");
    let cell_scope = scope();
    let session = Session {
        loads: &[],
        loaded: RefCell::new(None),
        root: directory.path(),
        manifest: &manifest,
        named: named(),
        metrics: DevMetrics::new([0; 32], cell_scope.clone()),
        scope: &cell_scope,
        url: "http://127.0.0.1:0",
        started_at: 0,
        actions: policy::dev_actions(),
        ungoverned: RefCell::new(None),
    };
    save(&session, &serving, &path);
    let sessions = [
        ACTOR.to_owned(),
        reviewer(ACTOR),
        CLERK.into(),
        BUYER.into(),
        OPS.into(),
    ]
    .into_iter()
    .map(|actor| (token(&actor), actor))
    .collect();
    Lab {
        _directory: directory,
        serving,
        principals: DevPrincipals::new(sessions).with_named(named()),
        metrics: DevMetrics::new([9; 32], scope()),
    }
}

fn save(session: &Session<'_>, serving: &Serving, cell: &Path) {
    let event = json!({ "ok": true, "gen": 1, "sourceDigest": "test", "artifacts": pack(),
        "sourceMap": [], "definitions": [gate()] });
    session
        .compiled(&mut Events { json: true }, serving, cell, &event)
        .map_err(|failure| failure.message)
        .expect("the save does not end the session")
        .expect("the save provisions the cell");
}

impl Lab {
    fn call(&self, actor: Option<&str>, method: &str, path: &str, body: &str) -> (u16, Value) {
        let guard = self.serving.read().expect("serving");
        let host: &DevHost<NativeEngine> = guard.as_ref().expect("host");
        let authorization = actor.map(|actor| format!("Bearer {}", token(actor)));
        let query = if path == "/v1/decisions" {
            format!("{SCOPE}&state=pending")
        } else {
            SCOPE.to_owned()
        };
        let response = http::answer(
            host,
            &self.principals,
            &self.metrics,
            None,
            &Request {
                method,
                path,
                query: &query,
                authorization: authorization.as_deref(),
                now: crate::dev::now_ms() / 1_000,
                body,
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap_or(Value::Null),
        )
    }

    fn act(&self, actor: &str, action: &str, input: &Value, key: &str) -> (u16, Value) {
        let body = json!({ "input": input, "idempotencyKey": key }).to_string();
        self.call(Some(actor), "POST", &format!("/v1/actions/{action}"), &body)
    }

    fn pending(&self, actor: &str) -> Value {
        let (status, page) = self.call(Some(actor), "GET", "/v1/decisions", "");
        assert_eq!(status, 200, "{actor}: {page}");
        page["total"].clone()
    }
}

/// `norn dev`'s own authorizer over these principals decides `operation`
/// for `actor` ([`LocalDeveloper::with_named`]).
fn authorize(actor: &str, operation: Operation) -> bool {
    LocalDeveloper::for_actor(ACTOR, policy::dev_actions())
        .with_named(named())
        .authorize(&AccessRequest {
            scope: &scope(),
            actor: Some(actor),
            operation,
        })
        .is_ok()
}

/// A declared role's action commits; an action of a role the principal
/// lacks is refused and writes nothing.
#[test]
fn a_named_principal_calls_only_what_its_roles_may() {
    let lab = lab();
    let document = json!({ "id": "d1", "owner": CLERK });
    let (status, recorded) = lab.act(CLERK, RECORD, &document, "record-d1");
    assert_eq!(
        (status, &recorded["outcome"]),
        (200, &json!("committed")),
        "{recorded}"
    );
    let (status, refused) = lab.act(BUYER, RECORD, &json!({ "id": "d2", "owner": BUYER }), "k2");
    assert_eq!(status, 403, "the buyer does not record: {refused}");
    let (status, refused) = lab.act(CLERK, APPROVE, &json!({ "id": "d1" }), "approve-clerk");
    assert_eq!(status, 403, "the clerk does not approve: {refused}");
    assert_eq!(lab.pending(ACTOR), json!(0), "nothing parked for the clerk");
}

/// The buyer's approve waits on the `admin`-only approval: neither the
/// clerk nor the buyer lists or decides it; a named `admin` and the
/// reviewer list it; the reviewer decides it.
#[test]
fn a_named_principal_decides_only_a_decision_its_roles_may() {
    let lab = lab();
    let (status, _) = lab.act(CLERK, RECORD, &json!({ "id": "d1", "owner": CLERK }), "r1");
    assert_eq!(status, 200);
    let (status, parked) = lab.act(BUYER, APPROVE, &json!({ "id": "d1" }), "approve-d1");
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"]
        .as_str()
        .expect("decision id")
        .to_owned();
    assert_eq!(
        lab.pending(CLERK),
        json!(0),
        "the clerk holds no admin role"
    );
    assert_eq!(lab.pending(BUYER), json!(0), "the requester never decides");
    assert_eq!(lab.pending(OPS), json!(1), "a named admin may decide");
    assert_eq!(lab.pending(&reviewer(ACTOR)), json!(1));
    let decide = format!("/v1/decisions/{id}:decide");
    let choice = |key: &str| json!({ "choice": "approve", "idempotencyKey": key }).to_string();
    let (status, refused) = lab.call(Some(CLERK), "POST", &decide, &choice("clerk"));
    assert_eq!(status, 404, "the clerk cannot decide it: {refused}");
    assert!(!authorize(
        CLERK,
        Operation::Decide {
            roles: vec!["admin".into()],
            law: None
        }
    ));
    let (status, decided) = lab.call(Some(&reviewer(ACTOR)), "POST", &decide, &choice("rev"));
    assert_eq!(status, 200, "{decided}");
}

/// Every hosted-admin operation is refused to a pack role; a named `admin`
/// holds the definition writes but provisions nothing; the developer and the
/// reviewer hold every one.
#[test]
fn a_named_principal_holds_no_hosted_admin_operation_its_roles_lack() {
    let lab = lab();
    let admin = || {
        [
            ("write", Operation::WriteDefinitions),
            ("upload", Operation::SyncSource("upload".into())),
            ("load", Operation::SyncSource("observations".into())),
            ("connection", Operation::ManageConnection("erp".into())),
            ("revoke", Operation::RevokeConnectionGrant("erp".into())),
            ("provision", Operation::ProvisionCell),
            ("run", Operation::StartRun("review.flow".into())),
        ]
    };
    for (shown, operation) in admin() {
        assert!(!authorize(CLERK, operation), "clerk: {shown}");
    }
    for (shown, operation) in admin() {
        let expected = shown != "provision";
        assert_eq!(authorize(OPS, operation), expected, "ops: {shown}");
    }
    for actor in [ACTOR.to_owned(), reviewer(ACTOR)] {
        for (shown, operation) in admin() {
            assert!(authorize(&actor, operation), "{actor}: {shown}");
        }
        assert!(authorize(&actor, Operation::ReadDefinitions));
    }
    assert!(authorize(CLERK, Operation::ReadDefinitions));
    assert!(authorize(CLERK, Operation::CallAction(RECORD.into())));
    assert!(!authorize("stranger", Operation::ReadDefinitions));

    // Over the routes: the clerk loads no observations (answered as a scope
    // with no cell) and admits no definition; the developer reaches both.
    let load = json!({ "profile": "synthetic-erpnext-three/1", "snapshot": [] }).to_string();
    let outside = crate::server::outside::<NativeEngine>(&Request {
        method: "POST",
        path: "/v1/observations:load",
        query: SCOPE,
        authorization: None,
        now: 1_800_000_000,
        body: &load,
    });
    let clerk = lab.call(Some(CLERK), "POST", "/v1/observations:load", &load);
    assert_eq!(
        clerk,
        (
            outside.status,
            serde_json::from_str(&outside.body).unwrap_or(Value::Null)
        )
    );
    let (status, refused) = lab.call(Some(ACTOR), "POST", "/v1/observations:load", &load);
    assert_eq!(status, 400, "the developer reaches the loader: {refused}");
    let approval = gate().to_string();
    let (status, refused) = lab.call(Some(CLERK), "POST", "/v1/definitions:admit", &approval);
    assert_eq!(status, 403, "{refused}");
    let (status, admitted) = lab.call(Some(ACTOR), "POST", "/v1/definitions:admit", &approval);
    assert_eq!(status, 200, "{admitted}");
}

/// [`DevPrincipals::with_named`] records a named principal's own roles for
/// the request, and the developer's every hosted role; [`LocalDeveloper`]
/// without `with_named` knows no named principal.
#[test]
fn the_resolver_records_each_principals_own_roles() {
    let lab = lab();
    let roles = |actor: &str| {
        let resolved = lab
            .principals
            .resolve(Some(&format!("Bearer {}", token(actor))), 0);
        assert_eq!(resolved.as_deref(), Some(actor));
        let roles = lab
            .principals
            .caller()
            .and_then(crate::policy::CallerSlot::current)
            .map(|caller| caller.roles);
        lab.principals.end_request();
        roles
    };
    assert_eq!(roles(CLERK), Some(vec!["warehouse_clerk".to_owned()]));
    assert_eq!(roles(OPS), Some(vec!["admin".to_owned()]));
    let developer = roles(ACTOR).expect("the developer's roles");
    assert!(
        ["viewer", "member", "admin", "owner"]
            .iter()
            .all(|role| developer.iter().any(|held| held == role)),
        "{developer:?}"
    );
    let bare = LocalDeveloper::for_actor(ACTOR, policy::dev_actions());
    let cell = scope();
    let request = |actor: &'static str| AccessRequest {
        scope: &cell,
        actor: Some(actor),
        operation: Operation::ReadDefinitions,
    };
    assert!(bare.authorize(&request(ACTOR)).is_ok());
    assert!(bare.authorize(&request(CLERK)).is_err());
}
