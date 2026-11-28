//! QUESTION-STEP-1 end-to-end integration tests through the DO fetch dispatch:
//! generic read-only `norn.question@1` perform steps in flows with
//! `RequiredSources` (`#1905`) freshness enforcement, `PRIVACY-1`/`PRIVACY-3`/
//! `PRIVACY-4` disclosure/markings refusal, `STEP-PIN-1` pin verification and
//! run-start drift detection, and no authority escalation from prior deciders.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "end-to-end question step integration tests"
)]

#[path = "../../norn-cell/tests/support/governed.rs"]
mod governed;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use governed::GovernedCommit as _;
use norn_api::analytics_routes::{
    EngineAnswer, EngineCall, ExportQuery, Transport, export_objects,
};
use norn_api::authz::{AccessRequest, Authorizer, Operation, ReadCapability, ReadRequest};
use norn_api::definition_routes::DefinitionKinds;
use norn_api::flow_question::question_step_codes;
use norn_api::host::DevHost;
use norn_api::object_routes::ObjectAccess;
use norn_api::pull::PullProfile;
use norn_api::question_routes::{QuestionRuntime, QuestionSnapshot, get_answer};
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_api::sources::{Sources, Step, Trigger};
use norn_api::timers::{DueWork, next_due};
use norn_cell::analytics::{AnalyticalEngineId, EngineCapabilities};
use norn_cell::compute::{ALL_MEMBERS, Engine, Freshness, QueryContext, QueryResult, QueryRow};
use norn_cell::engine::NativeEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::{
    CellError, CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, LedgerHead,
    LinkChange, MAIN_WORLD, ObjectRecord,
};
use norn_cell_do::policy::{CallerSlot, DEMO_GRANTS, ReadPolicy, collection_access};
use norn_cell_do::principal::{FIXTURE_ROLES, FixturePrincipals};
use norn_cell_do::questions::DerivedQuestions;
use norn_cell_do::run_due_with_runtime;
use norn_cell_do::server::{self, Response};
use norn_checker::EntityArtifactSetContext;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::{QuestionPlan, admit as admit_question_plan};
use norn_checker::receipt::ReceiptSources;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, Decimal, MetricDefinition, ObjectTypeName, QuestionIR,
    ResourceLimitProfile, RuntimeValue, UtcTimestamp,
};
use norn_kernel::authz::{
    self, ActorKind, AuthzSchema, CapabilityGrant, Pdp, PdpDecision, ResourceInput, ScopeKey,
};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, FactKey,
};
use norn_ui_checker::check_artifact_set_with_receipt;
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=dev";
const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
const MEMBER: Option<&str> = Some("Bearer fixture:member@demo");
/// Starts runs and reads its own run object, but is denied read access on
/// `canon.OrderLine` (the question's grain).
const BLIND: Option<&str> = Some("Bearer fixture:blind@demo");
/// Holds Population (not Values) on `canon.OrderLine`, and Population+Values on
/// Run objects.
const PARTIAL: Option<&str> = Some("Bearer fixture:partial@demo");

const TEST_FIXTURES: &[(&str, &[&str])] = &[
    ("admin@demo", &["admin"]),
    ("member@demo", &["member"]),
    ("blind@demo", &["member"]),
    ("partial@demo", &["member"]),
];

const T0: u64 = 1_794_484_800_000;
const T0_SECS: u64 = T0 / 1_000;
const SANDBOX_HOSTS: &str = "erpnext-rest";
const QUESTION_ID: &str = "freshness.order_line_qty";
const GUARD_FLOW_ID: &str = "ops.question_guard";
const DECIDE_FLOW_ID: &str = "ops.decide_then_question";
const TIMER_FLOW_ID: &str = "ops.timer_question_guard";
const MELT: &str = "constitution.Melt";
const MELT_ACTION: &str = "constitution.melt.convert";

const QUESTION_GUARD_IR: &str =
    include_str!("../../../tools/norn-ts/fixtures/flows/question-guard.ir.json");
const DECIDE_THEN_QUESTION_IR: &str =
    include_str!("../../../tools/norn-ts/fixtures/flows/decide-then-question.ir.json");
const TIMER_QUESTION_GUARD_IR: &str =
    include_str!("../../../tools/norn-ts/fixtures/flows/timer-question-guard.ir.json");

const ERPNEXT_CONFIG: &str = r#"[
  {"id": "erpnext.rest", "system": "erpnext", "label": "ERPNext REST",
   "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
   "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"],
   "headers": {"X-Frappe-Site-Name": "frontend"}}
]"#;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_artifact(path: &str) -> ArtifactEnvelope {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
}

fn scope_key() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        environment: Some("dev".into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

fn catalog_artifacts(with_erpnext: bool, with_markings: bool) -> Vec<ArtifactEnvelope> {
    let mut set: Vec<ArtifactEnvelope> = serde_json::from_str::<Vec<ArtifactEnvelope>>(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts")
    .into_iter()
    .filter(|a| a.kind() != ArtifactKind::Mapping)
    .collect();
    if with_erpnext {
        for kind in ["source", "mapping", "connector"] {
            set.push(load_artifact(&format!(
                "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.{kind}.json"
            )));
        }
        for name in [
            "source.Synced.interface",
            "source.Connection.entity",
            "source.SyncRun.entity",
            "source.Upload.entity",
            "norn.connect.source",
            "norn.connect.mapping",
        ] {
            set.push(load_artifact(&format!(
                "registry/packs/sources/artifacts/{name}.json"
            )));
        }
    }
    if with_markings {
        set.push(load_artifact(
            "registry/privacy/artifacts/privacy.markings.entity.json",
        ));
    }
    let constitution: Value = serde_json::from_str(include_str!(
        "../../norn-cell/tests/fixtures/constitution.json"
    ))
    .expect("constitution fixture");
    for doc in constitution["withLoss"].as_array().unwrap() {
        let envelope: ArtifactEnvelope = serde_json::from_value(doc.clone()).expect("melt");
        set.push(envelope);
    }
    set
}

/// ReadPolicy extended with Cedar action grants for `constitution.melt.convert`
/// and a per-subject read denial on `canon.OrderLine` for `blind@demo`.
struct QuestionTestPolicy {
    reads: ReadPolicy,
    action_pdp: Pdp,
    caller: Arc<CallerSlot>,
}

impl QuestionTestPolicy {
    fn compile(store: &CellStore<NativeEngine>, scope: &CellScope) -> Self {
        let reads =
            ReadPolicy::compile(store.model().object_types(), scope, &DEMO_GRANTS).expect("reads");
        let caller = reads.caller();
        let types: Vec<_> = store
            .model()
            .object_types()
            .filter(|object_type| object_type.name.as_str() == MELT)
            .cloned()
            .collect();
        let interfaces: Vec<_> = store.model().interfaces().cloned().collect();
        let constitution: Value = serde_json::from_str(include_str!(
            "../../norn-cell/tests/fixtures/constitution.json"
        ))
        .expect("constitution");
        let action: norn_ir::ActionRoot =
            serde_json::from_value(constitution["withLoss"][2]["payload"].clone()).unwrap();
        let projection = norn_projections::project_cedar_schema(
            &types,
            &interfaces,
            std::slice::from_ref(&action),
        );
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .expect("schema");
        let access: norn_ir::access::AccessIr = serde_json::from_value(json!({
            "accessVersion": "0.1",
            "policyId": "test.question.steps",
            "rules": [
                {"ruleId": "convert", "actions": [MELT_ACTION], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []},
                {"ruleId": "read_melt", "actions": ["norn.read"], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []}
            ]
        }))
        .unwrap();
        let grants: Vec<CapabilityGrant> = ["admin@demo", "member@demo"]
            .into_iter()
            .map(|subject| CapabilityGrant {
                subject_id: subject.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(scope_key()),
                capabilities: vec![MELT_ACTION.into(), "norn.read".into()],
                binding: None,
            })
            .collect();
        let action_pdp = Pdp::new(authz::compile(&schema, &access).unwrap(), &grants).unwrap();
        Self {
            reads,
            action_pdp,
            caller,
        }
    }

    fn decide_action(
        &self,
        action: &str,
        target: &FactKey,
        denial: ActionDenial,
    ) -> Result<(), ActionDenial> {
        let Some(caller) = self.caller.current().filter(|c| !c.roles.is_empty()) else {
            return Err(denial);
        };
        let req = authz::AccessRequest::new(
            &authz::VerifiedPrincipal::new(
                caller.subject_id,
                ActorKind::User,
                None,
                caller.roles,
                BTreeMap::new(),
            ),
            Some(scope_key()),
            action,
            action,
            ResourceInput {
                object_type: target.object_type.as_str().into(),
                id: target.key.clone(),
                scope: scope_key(),
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            authz::Binding::Unbound(authz::UnboundProfile::HostedAction),
        );
        match self.action_pdp.decide(&req) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(_) => Err(denial),
        }
    }
}

impl Authorizer for QuestionTestPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if matches!(request.operation, Operation::CallAction(_)) && request.actor.is_some() {
            return Ok(());
        }
        self.reads.authorize(request)
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if let Some(current) = self.caller.current()
            && request.access.resource().object_type == "canon.OrderLine"
            && (current.subject_id == "blind@demo"
                || (current.subject_id == "partial@demo"
                    && request.capability == ReadCapability::Values))
        {
            return Err(norn_api::authz::read_denied());
        }
        self.reads.read(request)
    }

    fn model_changed(&self, object_types: &[norn_ir::ObjectTypeDef]) {
        self.reads.model_changed(object_types);
    }

    fn admits_model(&self, object_types: &[norn_ir::ObjectTypeDef]) -> Result<(), Refusal> {
        self.reads.admits_model(object_types)
    }

    fn verified_principal(&self, actor: &str) -> Option<RunStarter> {
        self.reads.verified_principal(actor)
    }

    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.decide_action(
            &request.action.action_id,
            request.target,
            ActionDenial::Forbidden {
                reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
            },
        )
    }

    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.decide_action("norn.read", request.target, ActionDenial::Invisible)
    }

    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.decide_action(
            "norn.read",
            &FactKey::new(request.object_type.as_str(), "collection"),
            ActionDenial::Invisible,
        )
    }
}

fn commit_marked_order_line(
    store: &mut CellStore<NativeEngine>,
    scope: &CellScope,
    logical_time: u64,
) {
    let party = |key: &str, name: &str| FactWriteRow::Put {
        object_type: "canon.Party".into(),
        key: key.into(),
        values: BTreeMap::from([
            ("display_name".into(), RuntimeValue::from(name)),
            ("kind".into(), RuntimeValue::from("organization")),
        ]),
    };
    let link = |role: &str, target: &str| LinkChange::Add {
        object_type: "canon.OrderLine".into(),
        link: role.into(),
        source: "order-marked-1".into(),
        target: target.into(),
        values: BTreeMap::new(),
        target_type: Some("canon.Party".into()),
    };
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: scope.key(),
            logical_time,
            writes: vec![
                party("party-provider", "Provider"),
                party("party-receiver", "Receiver"),
                FactWriteRow::Put {
                    object_type: "canon.OrderLine".into(),
                    key: "order-marked-1".into(),
                    values: BTreeMap::from([
                        ("status".into(), RuntimeValue::from("promised")),
                        ("due_at".into(), RuntimeValue::from("2026-08-06T12:00:00Z")),
                        (
                            "quantity".into(),
                            RuntimeValue::Number("42".parse::<Decimal>().unwrap()),
                        ),
                        ("unit".into(), RuntimeValue::from("g")),
                        ("currency".into(), RuntimeValue::Null),
                    ]),
                },
            ],
            links: vec![
                link("provider", "party-provider"),
                link("receiver", "party-receiver"),
            ],
            events: vec![
                RuntimeValue::from_json(json!({
                    "type": "observation.recorded",
                    "mapped": [{
                        "objectType": "canon.OrderLine",
                        "key": "order-marked-1",
                        "propertyProvenance": {
                            "quantity": { "markings": ["pii.contact"] }
                        }
                    }]
                }))
                .unwrap(),
            ],
            ..CommitBatch::default()
        })
        .expect("commit marked order line");
}

struct QuestionCell {
    host: DevHost<NativeEngine>,
    principals: FixturePrincipals,
    caller: Arc<CallerSlot>,
    derived: DerivedQuestions,
    kinds: DefinitionKinds,
    scope: CellScope,
}

impl QuestionCell {
    fn open(with_erpnext: bool, with_markings: bool) -> Self {
        let artifacts = catalog_artifacts(with_erpnext, with_markings);
        let (base, candidate): (Vec<_>, Vec<_>) = artifacts
            .iter()
            .cloned()
            .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
        let to_rv = |items: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
            items
                .iter()
                .map(|a| {
                    RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("rv")
                })
                .collect()
        };
        let sources = ReceiptSources::new(&to_rv(&candidate), &to_rv(&base)).expect("sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("checked");
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let base_model = norn_api::kernel_set::serving_model(&artifacts).expect("serving model");
        let model =
            norn_api::action_admission::prepare_action_model(&base_model).expect("action model");
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
                    .expect("rv")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("utf8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        if with_markings {
            commit_marked_order_line(&mut store, &scope, T0);
        }
        let policy = QuestionTestPolicy::compile(&store, &scope);
        let caller = Arc::clone(&policy.caller);
        let principals = FixturePrincipals::new(TEST_FIXTURES, Arc::clone(&caller));
        let host = DevHost::from_store(store, scope.clone(), |cmd| cmd.payload.clone())
            .expect("host")
            .with_authorizer(policy);
        let derived = DerivedQuestions::new([7; 32], scope.clone());
        let kinds = DefinitionKinds::default()
            .with_roles(["viewer", "member", "admin", "owner"])
            .with_restater(norn_api::definition_routes::checker_restatement)
            .with_builtins(norn_cell::builtin_registered);
        Self {
            host,
            principals,
            caller,
            derived,
            kinds,
            scope,
        }
    }

    fn call_at(
        &self,
        method: &str,
        path: &str,
        query: &str,
        auth: Option<&str>,
        body: &str,
        now_secs: u64,
    ) -> (u16, Value) {
        let runtime = self
            .derived
            .questions_at(&self.host, now_secs.saturating_mul(1_000))
            .expect("runtime");
        let req = server::Request {
            method,
            path,
            query,
            authorization: auth,
            now: now_secs,
            body,
        };
        let Response { status, body } = server::handle_all(
            &self.host,
            &self.principals,
            Some(&runtime),
            &self.kinds,
            &req,
        );
        (status, serde_json::from_str(&body).unwrap())
    }

    fn call(&self, method: &str, path: &str, auth: Option<&str>, body: &str) -> (u16, Value) {
        self.call_at(method, path, SCOPE, auth, body, T0_SECS.saturating_add(5))
    }

    fn order_line_metric(&self) -> String {
        let (status, doc) = self.call("GET", "/v1/questions:metrics", ADMIN, "");
        assert_eq!(status, 200, "{doc}");
        doc["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["objectType"] == "canon.OrderLine" && m["expression"] == "quantity")
            .expect("canon.OrderLine.quantity metric")["name"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn install_order_line_question(&self, max_age_ms: u64, expected_version: Option<u64>) -> Value {
        self.install_order_line_question_with_filter(max_age_ms, expected_version, None)
    }

    fn install_order_line_question_with_filter(
        &self,
        max_age_ms: u64,
        expected_version: Option<u64>,
        filter: Option<Value>,
    ) -> Value {
        let metric = self.order_line_metric();
        let mut payload = json!({
            "irVersion": "0.1",
            "name": QUESTION_ID,
            "objectSet": "canon.OrderLine",
            "calendar": "iso_utc",
            "aggregate": [{"metric": metric}],
            "groupBy": [
                {"path": {"property": "due_at"}, "grain": "day"},
                {"path": {"property": "unit"}},
                {"path": {"property": "currency"}}
            ],
            "window": {"start": "2026-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": max_age_ms}
        });
        if let Some(filters) = filter {
            payload["filter"] = filters;
        }
        let mut body = json!({
            "kind": "question",
            "id": QUESTION_ID,
            "payload": payload,
        });
        if let Some(version) = expected_version {
            body["expectedVersion"] = json!(version);
        }
        let (status, installed) =
            self.call("POST", "/v1/definitions:install", ADMIN, &body.to_string());
        assert_eq!(status, 200, "{installed}");
        installed
    }

    fn install_flow(&self, flow_id: &str, title: &str, artifacts: Vec<Value>) {
        let mut body = json!({
            "kind": "flow",
            "id": flow_id,
            "payload": {
                "flow": { "id": flow_id, "version": "1.0.0", "title": title },
                "artifacts": artifacts,
            }
        });
        let (status, admitted) =
            self.call("POST", "/v1/definitions:admit", ADMIN, &body.to_string());
        assert_eq!(status, 200, "{admitted}");
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        let (status, installed) =
            self.call("POST", "/v1/definitions:install", ADMIN, &body.to_string());
        assert_eq!(status, 200, "{installed}");
    }

    fn install_guard_flow(&self, pin_expr: Option<&str>) {
        let mut artifacts: Vec<Value> = serde_json::from_str(QUESTION_GUARD_IR).unwrap();
        if let Some(pin) = pin_expr {
            let rule = &mut artifacts[1]["payload"];
            let rules = rule["graph"]["nodes"][1]["content"]["rules"]
                .as_array_mut()
                .unwrap();
            let start_row = rules
                .iter_mut()
                .find(|entry| entry["_id"] == "started/start/1")
                .unwrap();
            start_row["o_commands"] = json!(format!(
                "[{{kind: 'perform', runId: event.runId, step: 'check_question', function: 'norn.question', functionVersion: '1', idempotencyKey: event.runId + ':check_question', attempt: 1, input: {{check_question: {{question: '{QUESTION_ID}', pin: {pin}}}}}}}]"
            ));
            rule["outputSchema"]["properties"]["commands"]["items"]["properties"]["input"]["properties"]
                ["check_question"]["properties"]["pin"] = json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["artifactId", "version"],
                "properties": {
                    "artifactId": { "type": "string" },
                    "version": { "type": "string" },
                    "digest": { "type": "string" },
                    "definitionsVersion": { "type": "integer" }
                }
            });
        }
        self.install_flow(GUARD_FLOW_ID, "Guard a melt with a question", artifacts);
    }

    fn install_decide_then_question_flow(&self) {
        let artifacts: Vec<Value> = serde_json::from_str(DECIDE_THEN_QUESTION_IR).unwrap();
        self.install_flow(DECIDE_FLOW_ID, "Decide then read a question", artifacts);
    }

    fn install_timer_question_guard_flow(&self) {
        let artifacts: Vec<Value> = serde_json::from_str(TIMER_QUESTION_GUARD_IR).unwrap();
        self.install_flow(
            TIMER_FLOW_ID,
            "Sleep then guard a melt with a question",
            artifacts,
        );
    }

    fn install_guard_flow_with_params(&self, params_expr: &str, param_schema: Value) {
        let mut artifacts: Vec<Value> = serde_json::from_str(QUESTION_GUARD_IR).unwrap();
        let rule = &mut artifacts[1]["payload"];
        let rules = rule["graph"]["nodes"][1]["content"]["rules"]
            .as_array_mut()
            .unwrap();
        let start_row = rules
            .iter_mut()
            .find(|entry| entry["_id"] == "started/start/1")
            .unwrap();
        start_row["o_commands"] = json!(format!(
            "[{{kind: 'perform', runId: event.runId, step: 'check_question', function: 'norn.question', functionVersion: '1', idempotencyKey: event.runId + ':check_question', attempt: 1, input: {{check_question: {{question: '{QUESTION_ID}', params: {params_expr}}}}}}}]"
        ));
        rule["outputSchema"]["properties"]["commands"]["items"]["properties"]["input"]["properties"]
            ["check_question"]["properties"]["params"] = param_schema;
        self.install_flow(GUARD_FLOW_ID, "Guard a melt with a question", artifacts);
    }

    /// Install a new definitions revision that modifies only `erpnext.rest.mapping`
    /// (bump version `0.2.0` -> `0.2.1`), leaving the retained question and
    /// entity `model_hash` completely unchanged.
    fn upgrade_erpnext_mapping(&self) {
        let active = {
            let cell = self.host.cell().expect("cell");
            cell.store()
                .read()
                .installed_definitions(None)
                .expect("active")
                .expect("installed")
        };
        let mut artifacts =
            norn_api::kernel_set::retained_artifacts(&active.canonical_set).expect("retained");
        let mapping = artifacts
            .iter_mut()
            .find(|artifact| artifact.artifact_id == "erpnext.rest.mapping")
            .expect("erpnext.rest.mapping");
        mapping.version = "0.2.1".to_owned();

        let (base, candidate): (Vec<_>, Vec<_>) = artifacts
            .iter()
            .cloned()
            .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
        let to_rv = |items: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
            items
                .iter()
                .map(|a| {
                    RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("rv")
                })
                .collect()
        };
        let sources = ReceiptSources::new(&to_rv(&candidate), &to_rv(&base)).expect("sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("checked");
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let base_model = norn_api::kernel_set::serving_model(&artifacts).expect("serving model");
        let model =
            norn_api::action_admission::prepare_action_model(&base_model).expect("action model");
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
                    .expect("rv")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("utf8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: active.version.saturating_add(1),
            layout: None,
        };
        let upgraded = self
            .host
            .install_definitions(model, input, Some(active.version))
            .expect("upgrade mapping");
        assert_eq!(
            upgraded.model_hash, active.model_hash,
            "dependency-only mapping upgrade must keep model_hash unchanged"
        );
    }

    /// Run due work on the real production alarm/timer wake entrypoint
    /// (`norn_cell_do::run_due_with_runtime`) with NO active HTTP request in
    /// `self.caller`.
    fn run_due(&self, now_ms: u64) -> DueWork {
        assert_eq!(self.caller.current(), None);
        let runtime = self
            .derived
            .questions_at(&self.host, now_ms)
            .expect("runtime");
        run_due_with_runtime(
            &self.host,
            &self.caller,
            FIXTURE_ROLES,
            Some(&runtime),
            now_ms,
        )
        .expect("run_due_with_runtime")
    }

    fn sync_erpnext(&self, now_ms: u64) {
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/connectors/rest/erpnext.responses.json"),
            )
            .expect("responses"),
        )
        .expect("json");
        let mut pages: BTreeMap<String, (u16, Value)> = doc["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["dataset"].as_str().unwrap().to_owned(),
                    (
                        u16::try_from(p["status"].as_u64().unwrap()).unwrap(),
                        p["body"].clone(),
                    ),
                )
            })
            .collect();
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("profile");
        let secrets = BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]);
        let mut sources =
            Sources::with_allowed_hosts(&self.scope, ERPNEXT_CONFIG, &secrets, SANDBOX_HOSTS)
                .expect("src");
        sources
            .claim("erpnext.rest", Trigger::Manual, "user:admin@demo", now_ms)
            .expect("claim");
        let mut step = sources.step(&self.host, "erpnext.rest", None, now_ms);
        let mut t = now_ms;
        loop {
            match step {
                Step::Request(req) => {
                    let feed = profile
                        .feeds
                        .iter()
                        .find(|f| req.url.contains(f.request.path.as_str()))
                        .expect("feed");
                    let (status, body) = pages
                        .remove(&feed.dataset)
                        .unwrap_or_else(|| (200, json!({"data": []})));
                    t = t.saturating_add(1);
                    step = sources.step(
                        &self.host,
                        "erpnext.rest",
                        Some((status, body.to_string().as_bytes())),
                        t,
                    );
                }
                Step::Continue => {
                    t = t.saturating_add(1);
                    step = sources.step(&self.host, "erpnext.rest", None, t);
                }
                Step::Done(summary) => {
                    assert_eq!(summary.outcome, "ok", "{summary:#?}");
                    break;
                }
            }
        }
    }

    fn melt_status(&self) -> u16 {
        self.call("GET", "/v1/objects/constitution.Melt/melt-7", ADMIN, "")
            .0
    }
}

/// Positive: flow performs `norn.question@1` against a synced source within the
/// freshness bound, receives typed `Answered` with `queryPin`, routes in the
/// decide table, and completes the downstream action cleanly.
#[test]
fn fresh_question_step_answers_routes_and_completes() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);
    cell.sync_erpnext(T0);

    let (status, run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-fresh-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");
    assert_eq!(cell.melt_status(), 200);

    let (status, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-fresh-1",
        ADMIN,
        "",
    );
    assert_eq!(status, 200, "{run_obj}");
    assert_eq!(
        run_obj["values"]["slots"]["result"]["outcome"],
        json!("converted"),
        "{run_obj}"
    );
    assert!(
        !run_obj["values"]["slots"]["result"]["queryPin"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "Answered queryPin must be populated: {run_obj}"
    );
    assert_eq!(cell.caller.current(), None);
}

/// Negative 1 (`#1905` `RequiredSources`): unsynced/incomplete and aged/stale
/// required sources both settle as typed `StaleSource`, allowing the flow's
/// decide table to withhold downstream actions with zero business mutations.
#[test]
fn stale_or_unsynced_required_source_settles_stale_source_and_withholds_action() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);

    // 1a: Before any connector sync -> RequiredSources marks `canon.OrderLine` incomplete.
    let (status, incomplete_run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-unsynced-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{incomplete_run}");
    assert_eq!(
        incomplete_run["state"],
        json!("completed"),
        "{incomplete_run}"
    );
    let (_, unsynced_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-unsynced-1",
        ADMIN,
        "",
    );
    assert_eq!(
        unsynced_obj["values"]["slots"]["result"]["outcome"],
        json!("withheld_stale"),
        "{unsynced_obj}"
    );
    assert_eq!(
        cell.melt_status(),
        404,
        "downstream action must be withheld when required source is incomplete"
    );

    // 1b: After sync at T0, advancing wall clock past 60s maxAgeMs -> stale.
    cell.sync_erpnext(T0);
    let (status, stale_run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-stale-1" }).to_string(),
        T0_SECS.saturating_add(120),
    );
    assert_eq!(status, 201, "{stale_run}");
    assert_eq!(stale_run["state"], json!("completed"), "{stale_run}");
    let (_, stale_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-stale-1",
        ADMIN,
        "",
    );
    assert_eq!(
        stale_obj["values"]["slots"]["result"]["outcome"],
        json!("withheld_stale"),
        "{stale_obj}"
    );
    assert_eq!(
        cell.melt_status(),
        404,
        "downstream action must be withheld when required source is stale"
    );
}

/// Negative 2a: an unauthorized starter principal settles `norn.question@1` as
/// `Denied` without leaking any question values or firing downstream actions.
#[test]
fn unauthorized_starter_settles_denied_without_leaking_values() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);
    cell.sync_erpnext(T0);

    let (status, run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        BLIND,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-unauth-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("failed_denied"), "{run}");
    assert_eq!(run["terminal"], json!(true));
    assert_eq!(cell.melt_status(), 404);

    let (_, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-unauth-1",
        BLIND,
        "",
    );
    assert_eq!(
        run_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Denied" }),
        "must not leak question values or internal refusal detail: {run_obj}"
    );
    assert!(
        run_obj["values"]["slots"].get("bindings").is_none(),
        "no answer binding may be stored on Denied: {run_obj}"
    );
}

/// Negative 2b: a question touching a property carrying live restricted
/// markings (`PRIVACY-4`) settles as `Denied` without leaking values.
#[test]
fn restricted_markings_on_question_settle_denied_without_leaking_values() {
    let cell = QuestionCell::open(false, true);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);

    let (status, run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-marked-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("failed_denied"), "{run}");
    assert_eq!(cell.melt_status(), 404);

    let (_, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-marked-1",
        ADMIN,
        "",
    );
    assert_eq!(
        run_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Denied" }),
        "{run_obj}"
    );
}

/// Negative 3 (`STEP-PIN-1`): explicit pin mismatch and run-start definition
/// drift both fail closed with typed `Refused` codes without firing actions.
#[test]
fn step_pin_mismatch_and_definition_drift_refuse_fail_closed() {
    // 3a: Explicit pin mismatch (`version: "99"`).
    let pin_cell = QuestionCell::open(true, false);
    pin_cell.install_order_line_question(60_000, None);
    pin_cell.install_guard_flow(Some(
        "{artifactId: 'freshness.order_line_qty', version: '99'}",
    ));
    pin_cell.sync_erpnext(T0);

    let (status, pin_run) = pin_cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-pin-bad-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{pin_run}");
    assert_eq!(pin_run["state"], json!("failed_refused"), "{pin_run}");
    assert_eq!(pin_cell.melt_status(), 404);
    let (_, pin_obj) = pin_cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-pin-bad-1",
        ADMIN,
        "",
    );
    assert_eq!(
        pin_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Refused", "code": question_step_codes::PIN_MISMATCH }),
        "{pin_obj}"
    );

    // 3b: Retained question modified while run is parked on `ask_review`.
    let drift_cell = QuestionCell::open(true, false);
    drift_cell.install_order_line_question(60_000, None);
    drift_cell.install_decide_then_question_flow();
    drift_cell.sync_erpnext(T0);

    let (status, opened) = drift_cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        MEMBER,
        &json!({ "flow": DECIDE_FLOW_ID, "runId": "q-drift-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));

    // Update the retained question definition to revision 2 while parked.
    drift_cell.install_order_line_question(30_000, Some(1));

    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = drift_cell.call_at(
        "GET",
        "/v1/decisions",
        &pending,
        ADMIN,
        "",
        T0_SECS.saturating_add(6),
    );
    assert_eq!(page["total"], json!(1), "{page}");
    let decision_id = page["items"][0]["id"].as_str().unwrap().to_owned();

    let (status, decided) = drift_cell.call_at(
        "POST",
        &format!("/v1/decisions/{decision_id}:decide"),
        SCOPE,
        ADMIN,
        &json!({ "choice": "approve", "idempotencyKey": "k-drift-q" }).to_string(),
        T0_SECS.saturating_add(6),
    );
    assert_eq!(status, 200, "{decided}");
    assert_eq!(
        decided["run"]["state"],
        json!("failed_refused"),
        "{decided}"
    );
    assert_eq!(drift_cell.melt_status(), 404);
    let (_, drift_obj) = drift_cell.call(
        "GET",
        "/v1/objects/ops.DecideThenQuestionRun/q-drift-1",
        ADMIN,
        "",
    );
    assert_eq!(
        drift_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Refused", "code": question_step_codes::DEFINITION_CHANGED }),
        "{drift_obj}"
    );
}

/// Negative 4: a privileged decider approving an earlier `norn.decision@1` step
/// cannot escalate authority for a subsequent `norn.question@1` step; it
/// evaluates strictly under the run starter's authority and settles `Denied`.
#[test]
fn prior_decider_cannot_escalate_subsequent_question_step() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_decide_then_question_flow();
    cell.sync_erpnext(T0);

    let (status, opened) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        BLIND,
        &json!({ "flow": DECIDE_FLOW_ID, "runId": "q-no-escalate-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));

    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = cell.call_at(
        "GET",
        "/v1/decisions",
        &pending,
        ADMIN,
        "",
        T0_SECS.saturating_add(6),
    );
    assert_eq!(page["total"], json!(1), "{page}");
    let decision_id = page["items"][0]["id"].as_str().unwrap().to_owned();

    // Admin has full read & convert authority, but approval lends the run none.
    let (status, decided) = cell.call_at(
        "POST",
        &format!("/v1/decisions/{decision_id}:decide"),
        SCOPE,
        ADMIN,
        &json!({ "choice": "approve", "idempotencyKey": "k-no-esc" }).to_string(),
        T0_SECS.saturating_add(6),
    );
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["run"]["state"], json!("failed_denied"), "{decided}");
    assert_eq!(cell.melt_status(), 404);

    let (_, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.DecideThenQuestionRun/q-no-escalate-1",
        BLIND,
        "",
    );
    assert_eq!(
        run_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Denied" }),
        "{run_obj}"
    );
    assert_eq!(cell.caller.current(), None);
}

/// Negative 5 (Finding 3 — Run-binding privacy preservation): once a run
/// persists `Answered` outputs from `norn.question@1` into `run.slots.bindings`,
/// an ordinary Run reader who lacks source clearance on `canon.OrderLine`
/// cannot read those question outputs through object get, list, or history.
#[test]
fn ordinary_run_reader_without_source_clearance_cannot_read_question_bindings() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);
    cell.sync_erpnext(T0);

    let (status, run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-priv-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");

    // Authorized reader (ADMIN) sees the completed run and its bindings.
    let (status, admin_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-priv-1",
        ADMIN,
        "",
    );
    assert_eq!(status, 200, "{admin_obj}");
    assert_eq!(
        admin_obj["values"]["slots"]["result"]["outcome"],
        json!("converted"),
        "{admin_obj}"
    );

    // BLIND holds Population+Values on ops.QuestionGuardRun, but is denied
    // Population+Values on canon.OrderLine -> opaque 404 on get & history,
    // omitted from list.
    let (status, blind_get) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-priv-1",
        BLIND,
        "",
    );
    assert_eq!(status, 404, "BLIND must not read Run bindings: {blind_get}");
    let (status, blind_list) = cell.call("GET", "/v1/objects/ops.QuestionGuardRun", BLIND, "");
    assert_eq!(status, 200, "{blind_list}");
    assert!(
        !blind_list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "q-priv-1"),
        "BLIND must not see question-bearing run in list: {blind_list}"
    );
    let (status, blind_hist) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-priv-1/history",
        BLIND,
        "",
    );
    assert_eq!(status, 404, "BLIND must not read Run history: {blind_hist}");

    // PARTIAL holds Population on canon.OrderLine (denied Values) and
    // Population+Values on ops.QuestionGuardRun -> values withheld on get,
    // opaque 404 on history.
    let (status, partial_get) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-priv-1",
        PARTIAL,
        "",
    );
    assert_eq!(status, 200, "{partial_get}");
    assert_eq!(
        partial_get["withheld"],
        json!(true),
        "PARTIAL must have Run values withheld: {partial_get}"
    );
    assert!(
        partial_get["values"].is_null(),
        "PARTIAL must not receive Run bindings: {partial_get}"
    );
    let (status, partial_hist) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-priv-1/history",
        PARTIAL,
        "",
    );
    assert_eq!(
        status, 404,
        "PARTIAL must not read Run history: {partial_hist}"
    );
}

struct TestObjectAccess;

impl ObjectAccess for TestObjectAccess {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<authz::AccessRequest, Refusal> {
        let mut req = collection_access(scope, actor, capability, object_type);
        if let Some(row) = row {
            req.resource_mut().object_type.clone_from(&row.object_type);
            req.resource_mut().id.clone_from(&row.id);
        }
        Ok(req)
    }
}

struct PanicTransport;

impl Transport for PanicTransport {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::of(AnalyticalEngineId::DuckDb)
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, _call: &EngineCall) -> Option<EngineAnswer> {
        panic!("unauthorized export must be refused before reaching analytics transport");
    }
}

struct RunQuestionRt {
    plan: QuestionPlan,
    key: [u8; 32],
}

impl QuestionRuntime<NativeEngine> for RunQuestionRt {
    fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
        assert_eq!(question, "test.run_count");
        Ok(self.plan.clone())
    }
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                population_pin: format!("test:{actor}"),
                authorization_epoch: 1,
                mapping_pin: "test".to_owned(),
                now_ms: T0.saturating_add(10_000),
                source_cursors: BTreeMap::new(),
                watermarks: BTreeMap::new(),
            },
            cutoff: None,
        })
    }
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<authz::AccessRequest, Refusal> {
        let scope_key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        let (object_type, id) = row.map_or_else(
            || ("ops.QuestionGuardRun".to_owned(), question.to_owned()),
            |r| (r.object_type.clone(), r.id.clone()),
        );
        let cap = norn_cell_do::policy::capability_name(capability);
        Ok(authz::AccessRequest::new(
            &authz::VerifiedPrincipal::new(actor, ActorKind::User, None, vec![], BTreeMap::new()),
            Some(scope_key.clone()),
            cap,
            norn_cell_do::policy::READ_ACTION,
            ResourceInput {
                object_type,
                id,
                scope: scope_key,
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            authz::Binding::Unbound(authz::UnboundProfile::HostedRead),
        ))
    }
    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
    ) -> Result<authz::AccessRequest, Refusal> {
        Ok(collection_access(scope, actor, capability, object_type))
    }
    fn min_disclosed_bucket(&self) -> usize {
        1
    }
    fn handle_key(&self) -> &[u8] {
        &self.key
    }
    fn evaluate(
        &self,
        store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        context: QueryContext,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        let all_gate = ObjectRecord {
            object_type: "ops.QuestionGuardRun".to_owned(),
            id: ALL_MEMBERS.to_owned(),
            scope: context.scope.clone(),
            version: 0,
            values: BTreeMap::new(),
        };
        let all_ok = visible(&all_gate)?;
        let mut rows = BTreeMap::new();
        for row in store.read().list(MAIN_WORLD, "ops.QuestionGuardRun", 100)? {
            if all_ok && visible(&row)? {
                rows.insert(
                    row.id.clone(),
                    QueryRow {
                        bucket: RuntimeValue::String(row.id.clone()),
                        totals: vec!["1".parse::<Decimal>().unwrap()],
                        value: Some("1".parse::<Decimal>().unwrap()),
                        members: BTreeMap::new(),
                        suppressed: false,
                        null_counts: Vec::new(),
                        member_count: Some(1),
                        membership_digest: None,
                    },
                );
            }
        }
        let head = store.read().head_commit_seq()?;
        Ok(QueryResult {
            query_pin: "qp-run".to_owned(),
            definition_pin: "dp-run".to_owned(),
            engine: Engine::Cell,
            revision: LedgerHead::genesis(),
            effective_as_of: UtcTimestamp("2027-01-01T00:00:00Z".to_owned()),
            known_as_of: head,
            grain: ObjectTypeName("ops.QuestionGuardRun".to_owned()),
            lineage_ref: "lr-run".to_owned(),
            rows,
            freshness: Freshness::Exact,
            complete: true,
            context,
            exclusions: BTreeMap::new(),
            unlinked: 0,
            source_disagreements: None,
        })
    }
}

fn build_run_question_runtime() -> RunQuestionRt {
    let run_entity: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "ops.QuestionGuardRun.entity.test",
        "kind": "entity",
        "namespace": "ops",
        "name": "QuestionGuardRun",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "system", "sourcePath": "test" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "ops.QuestionGuardRun",
                "properties": [
                    { "id": "status", "type": { "kind": "text" } },
                    { "id": "occurred_at", "type": { "kind": "timestamp", "timezone": "utc" } },
                    {
                        "id": "weight",
                        "type": {
                            "kind": "decimal",
                            "scale": 2,
                            "additivity": { "kind": "additive" }
                        }
                    }
                ]
            }
        }
    }))
    .unwrap();
    let run_catalog = ScopedView::of(
        &run_entity,
        std::slice::from_ref(&run_entity),
        &[],
        &BTreeSet::new(),
    )
    .into_catalog();
    let run_metric: MetricDefinition = serde_json::from_value(json!({
        "name": "test.run_count",
        "label": "Run count",
        "type": "simple",
        "type_params": {
            "metric_aggregation_params": {
                "semantic_model": "ops.QuestionGuardRun",
                "agg": "sum",
                "agg_time_dimension": "occurred_at"
            },
            "expr": "weight"
        },
        "x-norn": {
            "irVersion": "0.1",
            "additivity": { "kind": "additive" },
            "timeDimension": { "property": "occurred_at" }
        }
    }))
    .unwrap();
    let run_q: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1",
        "name": "test.run_count",
        "objectSet": "ops.QuestionGuardRun",
        "aggregate": [{ "metric": "test.run_count" }],
        "groupBy": [{ "path": { "property": "status" } }],
        "window": { "start": "2026-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z" },
        "freshness": { "kind": "live", "maxAgeMs": 60000 },
        "calendar": "iso_utc"
    }))
    .unwrap();
    let plan = admit_question_plan(&[run_metric], &run_q, &run_catalog, 1_000).expect("run plan");
    RunQuestionRt {
        plan,
        key: [9_u8; 32],
    }
}

/// Negative 5a (Finding 2 — export & question-over-Run enforcement): an
/// unauthorized reader without source clearance on `canon.OrderLine` (`BLIND`
/// and `PARTIAL`) is denied raw export (`export_objects` -> 403) AND sees zero
/// Run rows when evaluating a question over `ops.QuestionGuardRun`, while an
/// authorized reader (`ADMIN`) sees the Run row.
#[test]
fn unauthorized_run_reader_denied_on_export_and_question_over_run() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);
    cell.sync_erpnext(T0);

    let (status, run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-priv-exp-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");

    // Finding 2a: GET /v1/runs/{id} denies BLIND and PARTIAL with opaque 404,
    // while ADMIN reads the run document.
    let (status, admin_run) = cell.call("GET", "/v1/runs/q-priv-exp-1", ADMIN, "");
    assert_eq!(status, 200, "{admin_run}");
    let (status, blind_run) = cell.call("GET", "/v1/runs/q-priv-exp-1", BLIND, "");
    assert_eq!(
        status, 404,
        "BLIND must not read GET /v1/runs/{{id}}: {blind_run}"
    );
    let (status, partial_run) = cell.call("GET", "/v1/runs/q-priv-exp-1", PARTIAL, "");
    assert_eq!(
        status, 404,
        "PARTIAL must not read GET /v1/runs/{{id}}: {partial_run}"
    );

    let export_query = ExportQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
        columns: "status".to_owned(),
        format: Some("jsonl".to_owned()),
    };
    for (label, auth) in [("BLIND", BLIND), ("PARTIAL", PARTIAL)] {
        let _req = norn_cell_do::principal::RequestScope::new(&cell.principals);
        let actor =
            norn_cell_do::principal::PrincipalResolver::resolve(&cell.principals, auth, T0_SECS);
        let err = export_objects(
            &cell.host,
            &export_query,
            actor.as_deref(),
            "ops.QuestionGuardRun",
            &TestObjectAccess,
            &PanicTransport,
        )
        .expect_err("unauthorized export must refuse");
        assert_eq!(
            err.status, 403,
            "{label} export must fail with 403: {err:?}"
        );
    }

    let run_rt = build_run_question_runtime();
    let scope_q = ScopeQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
    };
    let run_rows_for = |auth: Option<&str>| -> usize {
        let _req = norn_cell_do::principal::RequestScope::new(&cell.principals);
        let actor =
            norn_cell_do::principal::PrincipalResolver::resolve(&cell.principals, auth, T0_SECS);
        let ans = get_answer(
            &cell.host,
            &scope_q,
            actor.as_deref(),
            "test.run_count",
            &run_rt,
        )
        .expect("question answer");
        match ans.as_answer() {
            norn_api::question_routes::QuestionAnswer::Live(res) => res.rows.len(),
            norn_api::question_routes::QuestionAnswer::Historical(h) => h.result.rows.len(),
        }
    };
    assert_eq!(run_rows_for(ADMIN), 1, "ADMIN must see the Run row");
    assert_eq!(
        run_rows_for(BLIND),
        0,
        "BLIND must not see Run row via question evaluation"
    );
    assert_eq!(
        run_rows_for(PARTIAL),
        0,
        "PARTIAL must not see Run row via question evaluation"
    );
}

/// Negative 5b (Finding 3 — unsynced source ID redaction & persisted
/// StaleSource read restriction): an unsynced required source (`erpnext.rest`,
/// empty cursors/watermarks) MUST NOT leak its source ID to an unauthorized
/// starter (`MEMBER`, lacking `Sources`), AND when an authorized starter
/// (`ADMIN`, holding `Sources`) persists `StaleSource.sources`, later Run
/// readers without `Sources` (`MEMBER`) are denied reading that Run.
#[test]
fn unsynced_stale_source_does_not_leak_to_unauthorized_starter_or_run_reader() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_guard_flow(None);
    // Deliberately do NOT sync erpnext.rest — required source is unsynced.

    // 1. MEMBER lacks `Sources` -> StaleSource.sources MUST be empty `[]`!
    let (status, member_run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        MEMBER,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-stale-member-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{member_run}");
    assert_eq!(member_run["state"], json!("failed_stale"), "{member_run}");
    let (status, member_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-stale-member-1",
        MEMBER,
        "",
    );
    assert_eq!(status, 200, "{member_obj}");
    assert_eq!(
        member_obj["values"]["slots"]["failure"]["_tag"],
        json!("StaleSource"),
        "{member_obj}"
    );
    assert_eq!(
        member_obj["values"]["slots"]["failure"]["sources"],
        json!([]),
        "unsynced source ID must NEVER leak to unauthorized starter MEMBER: {member_obj}"
    );

    // 2. ADMIN holds `Sources` -> StaleSource.sources records the unsynced source,
    //    AND MEMBER (lacking `Sources`) is denied reading ADMIN's Run on BOTH
    //    GET /v1/objects and GET /v1/runs/{id}.
    let (status, admin_run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-stale-admin-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{admin_run}");
    assert_eq!(admin_run["state"], json!("failed_stale"), "{admin_run}");
    let (status, admin_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-stale-admin-1",
        ADMIN,
        "",
    );
    assert_eq!(status, 200, "{admin_obj}");
    let sources = admin_obj["values"]["slots"]["failure"]["sources"]
        .as_array()
        .unwrap();
    assert!(
        !sources.is_empty(),
        "ADMIN holding Sources sees offending unsynced source: {admin_obj}"
    );

    let (status, member_read_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-stale-admin-1",
        MEMBER,
        "",
    );
    assert_eq!(
        status, 404,
        "MEMBER without Sources must be denied reading StaleSource run object: {member_read_obj}"
    );
    let (status, member_read_run) = cell.call("GET", "/v1/runs/q-stale-admin-1", MEMBER, "");
    assert_eq!(
        status, 404,
        "MEMBER without Sources must be denied reading StaleSource GET /v1/runs/{{id}}: {member_read_run}"
    );
}

/// Negative 6 (Finding 4 — STEP-PIN-1 dependency-only drift): modifying a
/// mapping in the question's dependency closure (`erpnext.rest.mapping`) while
/// keeping both the retained question and `model_hash` unchanged causes an
/// in-flight parked `norn.question@1` step to settle `Refused` with
/// `question_step.definition_changed`.
#[test]
fn dependency_only_mapping_upgrade_drifts_in_flight_question_step() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_decide_then_question_flow();
    cell.sync_erpnext(T0);

    let (status, opened) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        MEMBER,
        &json!({ "flow": DECIDE_FLOW_ID, "runId": "q-dep-drift-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));

    // Upgrade only `erpnext.rest.mapping` while the run is parked on `ask_review`.
    cell.upgrade_erpnext_mapping();

    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = cell.call_at(
        "GET",
        "/v1/decisions",
        &pending,
        ADMIN,
        "",
        T0_SECS.saturating_add(6),
    );
    assert_eq!(page["total"], json!(1), "{page}");
    let decision_id = page["items"][0]["id"].as_str().unwrap().to_owned();

    let (status, decided) = cell.call_at(
        "POST",
        &format!("/v1/decisions/{decision_id}:decide"),
        SCOPE,
        ADMIN,
        &json!({ "choice": "approve", "idempotencyKey": "k-dep-drift" }).to_string(),
        T0_SECS.saturating_add(6),
    );
    assert_eq!(status, 200, "{decided}");
    assert_eq!(
        decided["run"]["state"],
        json!("failed_refused"),
        "{decided}"
    );
    assert_eq!(cell.melt_status(), 404);
    let (_, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.DecideThenQuestionRun/q-dep-drift-1",
        ADMIN,
        "",
    );
    assert_eq!(
        run_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Refused", "code": question_step_codes::DEFINITION_CHANGED }),
        "{run_obj}"
    );
}

/// Negative 7 (Finding 4 — parameter binding enforcement): dynamic `params`
/// that attempt to replace an admitted question filter predicate settle
/// `Refused { code: "question_step.invalid_param" }`, and undeclared `params`
/// settle `Refused { code: "question_step.unsupported_params" }` fail-closed.
#[test]
fn unauthorized_or_predicate_replacing_params_refuse_fail_closed() {
    // 7a: Predicate-replacing param (`status` already filtered by the question).
    let pred_cell = QuestionCell::open(true, false);
    pred_cell.install_order_line_question_with_filter(
        60_000,
        None,
        Some(json!([{
            "path": { "property": "status" },
            "operator": "eq",
            "value": "promised"
        }])),
    );
    pred_cell.install_guard_flow_with_params(
        "{status: 'cancelled'}",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["status"],
            "properties": { "status": { "type": "string" } }
        }),
    );
    pred_cell.sync_erpnext(T0);

    let (status, run) = pred_cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-param-pred-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("failed_refused"), "{run}");
    assert_eq!(pred_cell.melt_status(), 404);
    let (_, obj) = pred_cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-param-pred-1",
        ADMIN,
        "",
    );
    assert_eq!(
        obj["values"]["slots"]["failure"],
        json!({ "_tag": "Refused", "code": question_step_codes::INVALID_PARAM }),
        "{obj}"
    );

    // 7b: Undeclared param (`unit`) on a question with no declared parameter bindings.
    let undecl_cell = QuestionCell::open(true, false);
    undecl_cell.install_order_line_question(60_000, None);
    undecl_cell.install_guard_flow_with_params(
        "{unit: 'g'}",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["unit"],
            "properties": { "unit": { "type": "string" } }
        }),
    );
    undecl_cell.sync_erpnext(T0);

    let (status, run) = undecl_cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-param-undecl-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["state"], json!("failed_refused"), "{run}");
    assert_eq!(undecl_cell.melt_status(), 404);
    let (_, obj) = undecl_cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-param-undecl-1",
        ADMIN,
        "",
    );
    assert_eq!(
        obj["values"]["slots"]["failure"],
        json!({ "_tag": "Refused", "code": question_step_codes::UNSUPPORTED_PARAMS }),
        "{obj}"
    );
}

/// Negative 7c (Finding 4 — authorization-before-override ordering): an
/// unauthorized caller (`BLIND`) passing a predicate-colliding `params`
/// binding MUST receive `Denied` (NOT `question_step.invalid_param`), so
/// unauthorized callers cannot probe hidden question filter predicates.
#[test]
fn unauthorized_starter_denied_before_param_collision_check() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question_with_filter(
        60_000,
        None,
        Some(json!([{
            "path": { "property": "status" },
            "operator": "eq",
            "value": "promised"
        }])),
    );
    cell.install_guard_flow_with_params(
        "{status: 'cancelled'}",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["status"],
            "properties": { "status": { "type": "string" } }
        }),
    );
    cell.sync_erpnext(T0);

    let (status, blind_run) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        BLIND,
        &json!({ "flow": GUARD_FLOW_ID, "runId": "q-param-pred-blind-1" }).to_string(),
        T0_SECS.saturating_add(5),
    );
    assert_eq!(status, 201, "{blind_run}");
    assert_eq!(
        blind_run["state"],
        json!("failed_denied"),
        "unauthorized starter must receive Denied before param-collision checks: {blind_run}"
    );
    let (_, blind_obj) = cell.call(
        "GET",
        "/v1/objects/ops.QuestionGuardRun/q-param-pred-blind-1",
        BLIND,
        "",
    );
    assert_eq!(
        blind_obj["values"]["slots"]["failure"],
        json!({ "_tag": "Denied" }),
        "{blind_obj}"
    );
}

/// Positive (Finding 6 — durable timer wake path): a flow that sleeps on a
/// durable timer and then performs `norn.question@1` -> routes -> commits
/// `constitution.melt.convert` completes purely from the timer/alarm wake
/// (`run_due_with` under the recorded starter) with ZERO HTTP retry.
#[test]
fn timer_driven_question_to_action_flow_completes_on_wake_without_http_retry() {
    let cell = QuestionCell::open(true, false);
    cell.install_order_line_question(60_000, None);
    cell.install_timer_question_guard_flow();
    cell.sync_erpnext(T0);

    let start_s = T0_SECS.saturating_add(5);
    let (status, started) = cell.call_at(
        "POST",
        "/v1/runs",
        SCOPE,
        ADMIN,
        &json!({ "flow": TIMER_FLOW_ID, "runId": "q-timer-1" }).to_string(),
        start_s,
    );
    assert_eq!(status, 201, "{started}");
    assert_eq!(started["state"], json!("sleeping_wait_sync"), "{started}");
    assert_eq!(started["terminal"], json!(false));
    assert_eq!(cell.melt_status(), 404);
    assert_eq!(cell.caller.current(), None);

    let due = next_due(&cell.host).expect("next_due").expect("timer due");
    assert_eq!(due, start_s.saturating_mul(1_000).saturating_add(10_000));

    // Advance clock to the timer deadline and run ONLY the background alarm
    // wake path (zero HTTP retry!).
    let work = cell.run_due(due);
    assert_eq!(work.timers, 1, "{work:#?}");
    assert!(
        work.steps >= 1,
        "question step must dispatch on wake: {work:#?}"
    );
    assert!(work.failed.is_empty(), "{work:#?}");
    assert_eq!(work.next, None, "{work:#?}");
    assert_eq!(cell.caller.current(), None);

    // Downstream action committed and run completed purely from alarm wake.
    assert_eq!(cell.melt_status(), 200);
    let (status, run_doc) = cell.call("GET", "/v1/runs/q-timer-1", ADMIN, "");
    assert_eq!(status, 200, "{run_doc}");
    assert_eq!(run_doc["state"], json!("completed"), "{run_doc}");
    assert_eq!(run_doc["terminal"], json!(true));

    let (status, run_obj) = cell.call(
        "GET",
        "/v1/objects/ops.TimerQuestionGuardRun/q-timer-1",
        ADMIN,
        "",
    );
    assert_eq!(status, 200, "{run_obj}");
    assert_eq!(
        run_obj["values"]["slots"]["result"]["outcome"],
        json!("converted"),
        "{run_obj}"
    );
    assert!(
        !run_obj["values"]["slots"]["result"]["queryPin"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "Answered queryPin must be populated after timer wake: {run_obj}"
    );
}
