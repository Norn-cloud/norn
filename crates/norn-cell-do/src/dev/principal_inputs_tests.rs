//! PRINCIPAL-INPUT-1 (#1091) on `norn dev`: the call record's
//! `principalInputs` column joins the cell with the first saved set that
//! declares principal inputs and stays under every later save, so saving a
//! set without them (a rollback) installs and keeps the values already
//! stored, and a restarted session reopens the cell under the model its
//! installed revision recorded.
use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, RwLock};

use norn_api::authz::{AccessRequest, Authorizer, ReadRequest, RefusalDetailRequest};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::flow_run::RunStarter;
use norn_ir::RuntimeValue;
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionPrincipalInputs, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, PrincipalInputsRequest, UnresolvedPrincipalInput,
};
use serde_json::{Value, json};

use super::{admit, keeps};
use crate::dev::http::{self, Serving};
use crate::dev::policy::{self, DevMetrics, DevPrincipals, LocalDeveloper};
use crate::dev::{Events, Manifest, Session};
use crate::server::Request;

#[path = "../../tests/support/review_pack.rs"]
mod review_pack;
use review_pack::{APPROVE, admin_role, approval_rule, approve_action, review_base};

const ACTOR: &str = "local:developer";
const TOKEN: &str = "developer-token-0123456789";
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

fn scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

/// The review pack: with `ruled`, its approval pins the decision that reads
/// `principal.isAdmin`; without, no artifact declares a principal input.
fn pack(ruled: bool) -> Vec<Value> {
    let mut pack = review_base(&admin_role());
    if ruled {
        pack.extend([
            approval_rule("0.1.0", true),
            approve_action("0.2.0", Some("0.1.0")),
        ]);
    } else {
        pack.push(approve_action("0.1.0", None));
    }
    pack
}

/// Save `artifacts` through `norn dev`'s own save path
/// ([`Session::compiled`]): the compile event is admitted against what the
/// live cell serves ([`keeps`]), then provisioned or committed over the
/// active revision. The new version.
fn saved(session: &Session<'_>, serving: &Serving, cell: &Path, artifacts: &[Value]) -> u64 {
    let event = json!({ "ok": true, "gen": 1, "sourceDigest": "test", "artifacts": artifacts,
        "sourceMap": [], "definitions": [] });
    session
        .compiled(&mut Events { json: true }, serving, cell, &event)
        .map_err(|failure| failure.message)
        .expect("the save does not end the session")
        .expect("the save installs a revision")
        .version
}

/// The local developer with a resolver: `norn dev`'s own authorizer resolves
/// no principal input, so a call stores a value only under this one. Every
/// declared input resolves `true`.
struct Resolving(LocalDeveloper);

impl Authorizer for Resolving {
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        self.0.read(request)
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action(request)
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action_read(request)
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.0.action_read_type(request)
    }
    fn verified_principal(&self, actor: &str) -> Option<RunStarter> {
        self.0.verified_principal(actor)
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        self.0.disclose_refusal_detail(request)
    }
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        self.0.authorize(request)
    }
    fn principal_inputs(
        &self,
        request: &PrincipalInputsRequest<'_>,
    ) -> Result<ActionPrincipalInputs, UnresolvedPrincipalInput> {
        Ok(ActionPrincipalInputs {
            principal: request.actor.to_owned(),
            policy_version: "dev-test/1".to_owned(),
            subject: request.subject.clone(),
            values: request
                .inputs
                .iter()
                .map(|input| (input.name.clone(), RuntimeValue::Bool(true)))
                .collect(),
        })
    }
}

/// One request of the local developer against the serving cell.
fn request(host: &DevHost<NativeEngine>, method: &str, path: &str, body: &str) -> Value {
    let principals = DevPrincipals::new(vec![(TOKEN.into(), ACTOR.into())]);
    let metrics = DevMetrics::new([9; 32], scope());
    let response = http::answer(
        host,
        &principals,
        &metrics,
        None,
        &Request {
            method,
            path,
            query: SCOPE,
            authorization: Some(&format!("Bearer {TOKEN}")),
            // The session's own clock: a save after this request installs
            // at a later time than anything the request commits.
            now: crate::dev::now_ms() / 1_000,
            body,
        },
    );
    assert_eq!(response.status, 200, "{method} {path}: {}", response.body);
    serde_json::from_str(&response.body).expect("a JSON answer")
}

/// The `principalInputs` the call record under idempotency key `key` keeps.
fn recorded_inputs(host: &DevHost<NativeEngine>, key: &str) -> Value {
    let path = format!("/v1/objects/{}", norn_kernel::ACTION_CALL_TYPE);
    request(host, "GET", &path, "")["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|record| record["id"].as_str().is_some_and(|id| id.ends_with(key)))
        .expect("the stored call record")["values"]["principalInputs"]
        .clone()
}

#[test]
fn a_dev_cell_keeps_principal_inputs_through_a_rollback_and_a_restart() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let serving: Serving = Arc::new(RwLock::new(None));
    let actions = policy::dev_actions();
    let manifest: Manifest = serde_json::from_value(json!({
        "format": "norn-project/1", "name": "review", "sources": [],
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dev": { "port": 0, "actor": ACTOR }, "cell": "cell.sqlite"
    }))
    .expect("manifest");
    let cell_scope = scope();
    let session = Session {
        loads: &[],
        loaded: RefCell::new(None),
        root: directory.path(),
        manifest: &manifest,
        named: Arc::default(),
        metrics: crate::dev::policy::DevMetrics::new([0; 32], cell_scope.clone()),
        scope: &cell_scope,
        url: "http://127.0.0.1:0",
        started_at: 0,
        actions: Arc::clone(&actions),
        ungoverned: RefCell::new(None),
    };

    // The first save provisions the cell; nothing declares a principal input.
    let base = saved(&session, &serving, &path, &pack(false));
    assert!(!keeps(&serving));

    // A save that declares principal inputs: the column joins the cell.
    assert_eq!(saved(&session, &serving, &path, &pack(true)), base + 1);
    assert!(keeps(&serving));

    // A call stores a value in it: a document is recorded, then approved by
    // the decision that reads `principal.isAdmin`.
    {
        let mut guard = serving.write().expect("serving");
        let host = guard.take().expect("host");
        *guard = Some(host.with_authorizer(Resolving(LocalDeveloper::for_actor(
            ACTOR,
            Arc::clone(&actions),
        ))));
    }
    let stored = {
        let guard = serving.read().expect("serving");
        let host = guard.as_ref().expect("host");
        let act = |action: &str, input: Value, key: &str| {
            let body = json!({ "input": input, "idempotencyKey": key }).to_string();
            request(host, "POST", &format!("/v1/actions/{action}"), &body)
        };
        let recorded = act(
            "review.document.record",
            json!({ "id": "r1", "owner": ACTOR }),
            "record-r1",
        );
        assert_eq!(recorded["outcome"], "committed", "{recorded}");
        let approved = act(APPROVE, json!({ "id": "r1" }), "approve-r1");
        assert_eq!(approved["outcome"], "committed", "{approved}");
        let stored = recorded_inputs(host, "approve-r1");
        assert_eq!(stored["principal"], ACTOR, "{stored}");
        assert_eq!(stored["values"], json!({ "isAdmin": true }), "{stored}");
        stored
    };

    // The rollback: the set declares none again. The live cell keeps the
    // column, so the save installs and the stored value stays where it was.
    assert_eq!(saved(&session, &serving, &path, &pack(false)), base + 2);
    assert!(keeps(&serving));
    assert_eq!(
        recorded_inputs(
            serving.read().expect("serving").as_ref().expect("host"),
            "approve-r1"
        ),
        stored
    );

    // A restarted session reopens the cell under the model the installed
    // revision recorded, with the kept column and its value.
    drop(session);
    drop(serving);
    let (reopened, _loaded) = crate::dev::reopen(
        &path,
        &scope(),
        (ACTOR, &crate::dev::policy::NamedPrincipals::default()),
        &[],
        &actions,
    )
    .map_err(|failure| failure.message)
    .expect("the cell reopens");
    let host = reopened.expect("an installed cell");
    assert!(host.serving().expect("serving").principal_inputs);
    assert_eq!(
        host.active_definitions()
            .expect("definitions")
            .expect("active")
            .version,
        base + 2
    );
    assert_eq!(recorded_inputs(&host, "approve-r1"), stored);
}

/// A catalog the action policy could not compile is refused by dev admission
/// too, before anything is installed.
#[test]
fn dev_admission_refuses_a_catalog_its_access_rules_contradict() {
    let owners = json!({ "ruleId": "owners", "actions": [review_pack::APPROVE],
        "resourceType": review_pack::DOCUMENT,
        "scope": { "dimensions": ["org", "environment"] },
        "allOf": [{ "resource": { "name": "owner", "op": "eq",
            "value": { "principalAttribute": "id" } } }] });
    let with_catalog = |kind: &str| {
        let catalog = json!([{ "name": "id", "type": kind, "source": { "kind": "attribute" } }]);
        let mut pack = review_base(&catalog);
        pack[1]["payload"]["rules"]
            .as_array_mut()
            .expect("rules")
            .push(owners.clone());
        pack.push(approve_action("0.1.0", None));
        pack
    };
    let refused = admit(&with_catalog("long"), 1_000, false)
        .map(|_admitted| ())
        .map_err(|refused| refused.diagnostics)
        .expect_err("refused");
    assert_eq!(
        refused[0]["code"], "artifact_set.policy.principal_catalog_conflict",
        "{refused:?}"
    );
    assert!(admit(&with_catalog("string"), 1_000, false).is_ok());
}
