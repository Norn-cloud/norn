//! Compartmented need-to-know disclosure benchmark (#1874).
//!
//! Exercises incomparable flat compartments (`compartment.alpha`,
//! `compartment.beta`) and their conjunction (`compartment.alpha` +
//! `compartment.beta`) over a typed TypeScript pack compiled into
//! `fixtures/packs/compartmented-ifc/artifacts/`:
//! - Static analyzers (`structural`): Ring-0 admission, `RED-TEAM-1`,
//!   `ORIGIN-DATAFLOW-1`, `LAW-HONESTY-1`, and `LIFE-SOUND` (explicitly noting
//!   `lifecycle.soundness.completion_unknown` on guarded paths).
//! - Server-path & `SqliteCell` execution (`runtime-enforced`): Cedar PDP
//!   compiled from the pack's `AccessIr` governing ALL operations (host reads,
//!   `norn.reveal` on `norn.Marking`, action invocation, and action-reference
//!   slice reads), 4-principal compartment matrix (`no_grant`, `A`, `B`,
//!   `A+B`), ledgered reveals (verifying `marking.revealed` ledger entries on
//!   allow and zero entries on deny), scope mismatch refusal, absent-vs-
//!   invisible indistinguishability on both point reads and action references,
//!   positive `ACTION-REF-IFC-1` allow witness with committed link verification,
//!   marked predicate influence refusal leaving state unchanged, and
//!   conjunction taint propagation.
//!
//! Honest scope: models flat incomparable need-to-know compartments and their
//! conjunction; makes no claim of ordered Bell-LaPadula dominance.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "compartmented disclosure benchmark fixtures and assertions"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_api::action_admission::prepare_action_model;
use norn_api::authz::{
    AccessRequest as HostAccessRequest, Authorizer, ReadCapability, ReadRequest,
};
use norn_api::host::DevHost;
use norn_api::object_routes::{ObjectAccess, get_object, get_queried_object};
use norn_api::privacy::{NornRevealRequest, reveal};
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::{
    CellError, CellStore, InstalledDefinitionInput, MAIN_WORLD, Model, ObjectRecord, SqliteCell,
    StoreOptions,
};
use norn_checker::law_honesty::{LawEnforcementStatus, classify_law_honesty};
use norn_checker::red_team::analyze_red_team;
use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::access::AccessIr;
use norn_ir::canonical::canonical_json_of;
use norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactKind, DiagnosticSeverity, ObjectTypeDef,
    ResourceLimitProfile, RuntimeValue as V,
};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, AttrValue, AuthzSchema, Binding, CapabilityGrant, Pdp,
    PdpDecision, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal, compile_all_projected,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::pack_mbt_support::cedar_projection::{
    project_cedar_schema_with_catalog, project_host_access_schema,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, ActionRefusal, Command, DecisionRefusal, DecisionTurn, FactEvent,
    LogicalTime, TransitionGuard, World, commit_action, transition_guards,
};
use serde_json::{Value as Json, json};
use tempfile::TempDir;

const ORG: &str = "org_bench";
const OTHER_ORG: &str = "org_other";
const ENV: &str = "dev";
const DOSSIER: &str = "intel.Dossier";
const DISPATCH: &str = "intel.Dispatch";
const MARKING_TYPE: &str = "norn.Marking";
const READ_ACTION: &str = "norn.read";
const REVEAL_ACTION: &str = "norn.reveal";
const ALPHA: &str = "compartment.alpha";
const BETA: &str = "compartment.beta";
const RESTRICTED_CODE: &str = "RESTRICTED";
const SECRET: [u8; 32] = [0x4B; 32];

fn physical(org: &str) -> CellScope {
    CellScope {
        org_id: org.into(),
        environment_id: ENV.into(),
    }
}

fn scope_query(org: &str) -> ScopeQuery {
    ScopeQuery {
        org_id: Some(org.into()),
        environment_id: Some(ENV.into()),
    }
}

fn scope_key(org: &str) -> ScopeKey {
    ScopeKey {
        org: org.into(),
        environment: Some(ENV.into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

fn scope_key_from_map(scope: &BTreeMap<String, String>) -> ScopeKey {
    ScopeKey {
        org: scope.get("orgId").cloned().unwrap_or_default(),
        environment: scope.get("environmentId").cloned(),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

struct PackFixture {
    raw_json: Vec<Json>,
    raw_values: Vec<V>,
    envelopes: Vec<ArtifactEnvelope>,
    object_types: Vec<ObjectTypeDef>,
    actions: BTreeMap<String, ActionRoot>,
    guards: Vec<TransitionGuard>,
    laws: Vec<ArtifactEnvelope>,
    admission_receipt: String,
    receipt_sha256: String,
    pdp: Arc<Pdp>,
}

fn pack_artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/packs/compartmented-ifc/artifacts")
}

fn build_fixture_pdp(
    object_types: &[ObjectTypeDef],
    action_roots: &[ActionRoot],
    policies: &[AccessIr],
) -> Arc<Pdp> {
    let projection = project_cedar_schema_with_catalog(object_types, &[], &[], action_roots);
    let host_schema = project_host_access_schema(object_types);
    let schema =
        AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
            .expect("projected Cedar schema");
    let policy_set =
        compile_all_projected(&schema, policies, &projection).expect("compile AccessIr to Cedar");
    let home = scope_key(ORG);
    let all_caps: Vec<String> = action_roots
        .iter()
        .map(|a| a.action_id.clone())
        .chain([READ_ACTION.to_owned(), REVEAL_ACTION.to_owned()])
        .collect();
    let grants: Vec<CapabilityGrant> = [
        "no_grant",
        "principal_a",
        "principal_b",
        "principal_ab",
        "approver_ab",
    ]
    .into_iter()
    .map(|subject| CapabilityGrant {
        subject_id: subject.to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(home.clone()),
        capabilities: all_caps.clone(),
        binding: None,
    })
    .collect();
    Arc::new(Pdp::new(policy_set, &grants).expect("build Cedar PDP"))
}

fn load_pack_fixture() -> PackFixture {
    let dir = pack_artifact_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read pack artifact dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "compiled pack artifacts must exist");

    let mut raw_json = Vec::with_capacity(paths.len());
    let mut raw_values = Vec::with_capacity(paths.len());
    let mut envelopes = Vec::with_capacity(paths.len());
    let mut object_types = Vec::new();
    let mut actions = BTreeMap::new();
    let mut laws = Vec::new();
    let mut policies: Vec<AccessIr> = Vec::new();

    for path in paths {
        let text = fs::read_to_string(&path).expect("read artifact");
        let val: Json = serde_json::from_str(&text).expect("parse artifact JSON");
        raw_json.push(val.clone());
        raw_values.push(V::from_json(val.clone()).expect("runtime value"));
        let payload = val.get("payload").cloned().unwrap_or(Json::Null);
        let env: ArtifactEnvelope = serde_json::from_value(val).expect("decode envelope");
        match env.kind() {
            ArtifactKind::Entity => {
                if let Some(ot) = payload.get("objectType") {
                    object_types.push(serde_json::from_value(ot.clone()).expect("objectType"));
                }
            }
            ArtifactKind::Action => {
                let action: ActionRoot = serde_json::from_value(payload).expect("ActionRoot");
                actions.insert(action.action_id.clone(), action);
            }
            ArtifactKind::Law => laws.push(env.clone()),
            ArtifactKind::Policy => {
                let access: AccessIr = serde_json::from_value(payload).expect("AccessIr");
                policies.push(access);
            }
            ArtifactKind::Relation
            | ArtifactKind::Rule
            | ArtifactKind::Connector
            | ArtifactKind::Widget
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Derived
            | ArtifactKind::SourceSchema
            | ArtifactKind::Mapping
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Labels
            | ArtifactKind::Model => {}
        }
        envelopes.push(env);
    }

    // Ring-0 admission produces the exact receipt installed into the cell store.
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let sources = ReceiptSources::new(&raw_values, &[]).expect("receipt sources");
    let checked =
        check_artifact_set_with_receipt(&sources, &ctx, &ResourceLimitProfile::norn_limits_v1())
            .expect("Ring-0 admission");
    assert!(checked.receipt.admitted, "fixture pack must pass Ring-0");
    let admission_receipt =
        String::from_utf8(checked.receipt.canonical_json().expect("receipt json")).expect("utf8");
    let receipt_sha256 = checked.receipt.content_sha256().expect("receipt sha256");

    let action_roots: Vec<ActionRoot> = actions.values().cloned().collect();
    let guards = transition_guards(actions.values());
    let pdp = build_fixture_pdp(&object_types, &action_roots, &policies);

    PackFixture {
        raw_json,
        raw_values,
        envelopes,
        object_types,
        actions,
        guards,
        laws,
        admission_receipt,
        receipt_sha256,
        pdp,
    }
}

fn principal_roles(actor: &str) -> Option<Vec<String>> {
    match actor {
        "no_grant" => Some(vec!["viewer".into()]),
        "principal_a" => Some(vec!["clear_alpha".into()]),
        "principal_b" => Some(vec!["clear_beta".into()]),
        "principal_ab" | "approver_ab" => Some(vec!["clear_ab".into()]),
        _ => None,
    }
}

fn code_attributes_from_values(
    object_type: &str,
    values: Option<&BTreeMap<String, V>>,
    default_code: Option<&str>,
) -> BTreeMap<String, AttrValue> {
    let mut attrs = BTreeMap::new();
    if object_type == DOSSIER {
        if let Some(code) = values.and_then(|v| v.get("code")).and_then(V::as_str) {
            attrs.insert("code".into(), AttrValue::String(code.to_owned()));
        } else if let Some(code) = default_code {
            attrs.insert("code".into(), AttrValue::String(code.to_owned()));
        }
    }
    attrs
}

/// Trusted server mapper constructing Cedar PDP read requests with verified
/// principal roles and committed row attributes (zero hard-coded bypasses).
struct CompartmentedAccess;

impl ObjectAccess for CompartmentedAccess {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        _capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let Some(roles) = principal_roles(actor) else {
            return Err(Refusal::new(
                http::StatusCode::FORBIDDEN,
                "authz.denied",
                "denied",
                "unknown principal",
            ));
        };
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        let concrete_type = row.map_or(object_type, |r| r.object_type.as_str());
        let attributes = code_attributes_from_values(
            concrete_type,
            row.map(|r| &r.values),
            row.is_none().then_some("PUBLIC"),
        );
        Ok(PdpRequest::new(
            &VerifiedPrincipal::new(actor, ActorKind::User, None, roles, BTreeMap::new()),
            Some(key.clone()),
            READ_ACTION,
            READ_ACTION,
            ResourceInput {
                object_type: concrete_type.into(),
                id: row.map_or(object_type, |r| r.id.as_str()).into(),
                scope: key,
                attributes,
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        ))
    }
}

/// Cedar-backed authorizer governing BOTH server reads/reveals (`DevHost`) and
/// kernel action commits/slice reads (`SqliteCell`) exclusively through the
/// compiled `AccessIr` PDP.
struct CompartmentedPolicy {
    pdp: Arc<Pdp>,
}

impl CompartmentedPolicy {
    fn decide_read(
        &self,
        actor: &str,
        sk: ScopeKey,
        object_type: &str,
        id: &str,
        attributes: BTreeMap<String, AttrValue>,
    ) -> bool {
        let Some(roles) = principal_roles(actor) else {
            return false;
        };
        let principal =
            VerifiedPrincipal::new(actor, ActorKind::User, None, roles, BTreeMap::new());
        let pdp_req = PdpRequest::new(
            &principal,
            Some(sk.clone()),
            READ_ACTION,
            READ_ACTION,
            ResourceInput {
                object_type: object_type.to_owned(),
                id: id.to_owned(),
                scope: sk,
                attributes,
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        );
        self.pdp.decide(&pdp_req) == PdpDecision::Allow
    }
}

impl Authorizer for CompartmentedPolicy {
    fn authorize(&self, req: &HostAccessRequest<'_>) -> Result<(), Refusal> {
        let valid_scope = req.scope.org_id == ORG && req.scope.environment_id == ENV;
        let known_actor = req
            .actor
            .is_some_and(|actor| principal_roles(actor).is_some());
        if valid_scope && known_actor {
            Ok(())
        } else {
            Err(Refusal::new(
                http::StatusCode::FORBIDDEN,
                "authz.denied",
                "denied",
                "host authorization denied",
            ))
        }
    }

    fn read(&self, req: &ReadRequest<'_>) -> Result<(), Refusal> {
        match self.pdp.decide(req.access) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(_) => Err(Refusal::invalid("cedar read policy denied")),
        }
    }

    fn reveal(&self, actor: &str, marking_id: &str) -> bool {
        let Some(roles) = principal_roles(actor) else {
            return false;
        };
        let key = scope_key(ORG);
        let principal =
            VerifiedPrincipal::new(actor, ActorKind::User, None, roles, BTreeMap::new());
        let pdp_req = PdpRequest::new(
            &principal,
            Some(key.clone()),
            REVEAL_ACTION,
            REVEAL_ACTION,
            ResourceInput {
                object_type: MARKING_TYPE.to_owned(),
                id: marking_id.to_owned(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::MarkingDecision),
        );
        self.pdp.decide_marking(&pdp_req, marking_id, REVEAL_ACTION) == PdpDecision::Allow
    }
}

impl ActionAuthorizer for CompartmentedPolicy {
    fn authorize(&self, req: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let Some(roles) = principal_roles(req.actor) else {
            return Err(ActionDenial::Forbidden {
                reason: "unknown principal".into(),
            });
        };
        let sk = scope_key_from_map(req.scope);
        let principal =
            VerifiedPrincipal::new(req.actor, ActorKind::User, None, roles, BTreeMap::new());
        let attributes =
            code_attributes_from_values(req.target.object_type.0.as_str(), req.target_values, None);
        let pdp_req = PdpRequest::new(
            &principal,
            Some(sk.clone()),
            &req.action.action_id,
            &req.action.action_id,
            ResourceInput {
                object_type: req.target.object_type.0.clone(),
                id: req.target.key.clone(),
                scope: sk,
                attributes,
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        );
        match self.pdp.decide(&pdp_req) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(_) => Err(ActionDenial::Forbidden {
                reason: "cedar policy denied".into(),
            }),
        }
    }

    fn authorize_read(&self, req: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let sk = scope_key_from_map(req.scope);
        let attributes =
            code_attributes_from_values(req.target.object_type.0.as_str(), req.target_values, None);
        if self.decide_read(
            req.actor,
            sk,
            req.target.object_type.0.as_str(),
            &req.target.key,
            attributes,
        ) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn authorize_read_type(
        &self,
        req: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        let sk = scope_key_from_map(req.scope);
        let attributes =
            code_attributes_from_values(req.object_type.0.as_str(), None, Some("PUBLIC"));
        if self.decide_read(
            req.actor,
            sk,
            req.object_type.0.as_str(),
            req.object_type.0.as_str(),
            attributes,
        ) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn supports_reference_lineage(&self) -> bool {
        true
    }
}

fn open_cell_and_model(fixture: &PackFixture) -> (TempDir, PathBuf, Model) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("compartmented.sqlite");
    let base_model = Model::from_definitions(&fixture.object_types, &[])
        .expect("pack model")
        .with_governing_laws(&fixture.laws)
        .expect("governing laws");
    let model = prepare_action_model(&base_model).expect("prepare action model");
    let canonical_set =
        String::from_utf8(canonical_json_of(&fixture.envelopes).expect("canonical set"))
            .expect("utf8");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    let key = physical(ORG).key();
    store
        .install_definitions(
            model.clone(),
            InstalledDefinitionInput {
                canonical_set,
                registry: vec![],
                receipt_sha256: fixture.receipt_sha256.clone(),
                admission_receipt: Some(fixture.admission_receipt.clone()),
                readmission: None,
                diff: json!({}),
                installed_at: 1,
                layout: None,
            },
            Some(0),
            &key,
        )
        .expect("install definitions with Ring-0 admission receipt");
    (dir, path, model)
}

struct ActionEnv<'a> {
    model: &'a Model,
    fixture: &'a PackFixture,
    policy: &'a CompartmentedPolicy,
}

fn run_action_in_org(
    cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    env: &ActionEnv<'_>,
    action: &ActionRoot,
    org: &str,
    actor: &str,
    idem: &str,
    input: Json,
) -> Result<norn_kernel::ActionOutcome, Box<ActionRefusal<CellError>>> {
    // Bind pack-wide transition guards (matching production `action_admission.rs`).
    let catalog = env
        .model
        .type_catalog()
        .with_governing_laws(env.model.law_artifacts())
        .with_transition_guards(&env.fixture.guards);
    let Ok(parsed) = serde_json::from_value(input) else {
        return Err(Box::new(ActionRefusal::InvalidInput {
            field: "input".into(),
        }));
    };
    let req = ActionCall {
        action,
        action_artifact_id: &format!("{}.action", action.action_id),
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), org.to_owned()),
            ("environmentId".to_owned(), ENV.to_owned()),
        ]),
        actor: actor.to_owned(),
        approved_intent: None,
        input: parsed,
        idempotency_key: idem.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: idem.to_owned(),
            now: LogicalTime::from_millis(1_793_702_400_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    };
    commit_action(cell, &catalog, &req, env.policy).map_err(Box::new)
}

fn run_action(
    cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    env: &ActionEnv<'_>,
    action: &ActionRoot,
    actor: &str,
    idem: &str,
    input: Json,
) -> Result<norn_kernel::ActionOutcome, Box<ActionRefusal<CellError>>> {
    run_action_in_org(cell, env, action, ORG, actor, idem, input)
}

fn register_dossier(
    cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    env: &ActionEnv<'_>,
    id: &str,
    code: &str,
) {
    run_action(
        cell,
        env,
        &env.fixture.actions["intel.dossier.register"],
        "principal_ab",
        &format!("reg-{id}"),
        json!({
            "id": id,
            "code": code,
            "alpha_brief": "alpha-secret-payload",
            "beta_brief": "beta-secret-payload",
            "joint_brief": "joint-ab-secret-payload"
        }),
    )
    .expect("register dossier");
}

fn seed_active_dossier(
    cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    env: &ActionEnv<'_>,
    id: &str,
    code: &str,
) {
    register_dossier(cell, env, id, code);
    run_action(
        cell,
        env,
        &env.fixture.actions["intel.dossier.activate"],
        "approver_ab",
        &format!("act-{id}"),
        json!({"id": id}),
    )
    .expect("activate dossier");
}

fn read_reveal_ledger_events(path: &Path, model: &Model) -> Vec<(String, String, String)> {
    let store = CellStore::open(path, model.clone(), StoreOptions::default()).expect("open store");
    let entries = store.read().ledger_page(0, 500).expect("read ledger page");
    entries
        .into_iter()
        .filter(|e| e.event.get("type").and_then(V::as_str) == Some("marking.revealed"))
        .map(|e| {
            let actor = e
                .event
                .get("actor")
                .and_then(V::as_str)
                .unwrap_or_default()
                .to_owned();
            let object = e
                .event
                .get("key")
                .and_then(V::as_str)
                .unwrap_or_default()
                .to_owned();
            let property = e
                .event
                .get("property")
                .and_then(V::as_str)
                .unwrap_or_default()
                .to_owned();
            (actor, object, property)
        })
        .collect()
}

#[test]
fn compartmented_pack_passes_admission_red_team_origin_dataflow_and_life_sound() {
    let fixture = load_pack_fixture();
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let sources = ReceiptSources::new(&fixture.raw_values, &[]).expect("receipt sources");
    let checked =
        check_artifact_set_with_receipt(&sources, &ctx, &ResourceLimitProfile::norn_limits_v1())
            .expect("Ring-0 receipt check");

    let errors: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "expected 0 Ring-0 errors, got {errors:#?}"
    );
    assert!(checked.receipt.admitted, "pack must be admitted");

    // LIFE-SOUND honesty: guarded `intel.Dispatch` transition reports Unknown cleanly.
    assert!(
        checked.diagnostics.iter().any(|d| {
            d.code == "lifecycle.soundness.completion_unknown"
                && d.artifact_id.as_deref() == Some("intel.Dispatch.entity")
        }),
        "guarded lifecycle path must honestly surface LIFE-SOUND Unknown"
    );

    // RED-TEAM-1 (structural): 0 scope confinement, 0 unauthenticated settlements, 0 self-ratification.
    let report = analyze_red_team(&fixture.envelopes);
    assert!(report.scope_confinement.is_empty(), "{report:#?}");
    assert!(report.unauthenticated_settlements.is_empty(), "{report:#?}");
    assert!(report.self_ratifiable_transitions.is_empty(), "{report:#?}");

    // LAW-HONESTY-1 (structural): every pack law classifies cleanly as enforced.
    let honesty = classify_law_honesty(&fixture.envelopes);
    assert_eq!(honesty.len(), 1);
    assert_eq!(honesty[0].enforcement, LawEnforcementStatus::Enforced);

    // ORIGIN-DATAFLOW-1 (structural negative witness): derive an update_only mutation
    // directly from the compiled `intel.dossier.register.action` artifact and verify
    // Ring-0 emits the exact `TARGET_NOT_WRITABLE` diagnostic for `alpha_brief`.
    let mut tampered = fixture.raw_values.clone();
    let mut tamper_action = fixture
        .raw_json
        .iter()
        .find(|v| v["artifactId"] == "intel.dossier.register.action")
        .expect("compiled register action")
        .clone();
    tamper_action["artifactId"] = json!("intel.dossier.tamper.action");
    tamper_action["name"] = json!("Tamper immutable alpha brief");
    tamper_action["payload"]["actionId"] = json!("intel.dossier.tamper");
    tamper_action["payload"]["writes"][0]["change"] = json!("modified");
    tamper_action["payload"]["effectProgram"]["writeMode"] = json!({"kind": "update_only"});
    tamper_action["payload"]
        .as_object_mut()
        .expect("payload object")
        .remove("derive");
    tampered.push(V::from_json(tamper_action).expect("runtime tamper action"));

    let tamper_sources = ReceiptSources::new(&tampered, &[]).expect("tamper sources");
    let tamper_checked = check_artifact_set_with_receipt(
        &tamper_sources,
        &ctx,
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("tamper check");
    assert!(!tamper_checked.receipt.admitted);
    assert!(
        tamper_checked.diagnostics.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error
                && d.code == "artifact_set.action.target_not_writable"
                && d.artifact_id.as_deref() == Some("intel.dossier.tamper.action")
                && d.message.contains("alpha_brief")
                && d.message.contains("immutableAfterCreate")
        }),
        "expected exact immutableAfterCreate diagnostic for alpha_brief, got {:#?}",
        tamper_checked.diagnostics
    );
}

fn assert_matrix_point_reads_and_reveals(host: &DevHost<norn_cell::engine::NativeEngine>) {
    for (actor, can_a, can_b, can_ab) in [
        ("no_grant", false, false, false),
        ("principal_a", true, false, false),
        ("principal_b", false, true, false),
        ("principal_ab", true, true, true),
    ] {
        let doc = get_queried_object(
            host,
            &scope_query(ORG),
            Some(actor),
            DOSSIER,
            "D1",
            &CompartmentedAccess,
        )
        .expect("projected read");
        let serialized = serde_json::to_value(&doc).expect("serialize");
        let text = serialized.to_string();
        assert!(!text.contains("alpha-secret-payload"));
        assert!(!text.contains("beta-secret-payload"));
        assert!(!text.contains("joint-ab-secret-payload"));
        assert_eq!(serialized["masked"]["alpha_brief"]["revealable"], can_a);
        assert_eq!(serialized["masked"]["beta_brief"]["revealable"], can_b);
        assert_eq!(serialized["masked"]["joint_brief"]["revealable"], can_ab);

        for (prop, secret, allowed) in [
            ("alpha_brief", "alpha-secret-payload", can_a),
            ("beta_brief", "beta-secret-payload", can_b),
            ("joint_brief", "joint-ab-secret-payload", can_ab),
        ] {
            let before_head = host.verified_head().expect("head before").seq;
            let req = NornRevealRequest {
                property: prop.into(),
                purpose: Some("need-to-know benchmark".into()),
                idempotency_key: format!("rev-{actor}-{prop}"),
            };
            let res = reveal(
                host,
                &scope_query(ORG),
                Some(actor),
                (DOSSIER, "D1"),
                &req,
                &CompartmentedAccess,
            );
            let after_head = host.verified_head().expect("head after").seq;
            if allowed {
                assert_eq!(res.expect("cleared reveal").value, V::from(secret));
                assert_eq!(
                    after_head,
                    before_head.saturating_add(1),
                    "allowed reveal must append one ledger entry"
                );
            } else {
                assert_eq!(
                    res.expect_err("denied reveal").status,
                    http::StatusCode::FORBIDDEN
                );
                assert_eq!(
                    after_head, before_head,
                    "denied reveal must append zero ledger entries"
                );
            }
        }
    }
}

fn assert_host_scope_and_indistinguishability(host: &DevHost<norn_cell::engine::NativeEngine>) {
    // Scope mismatch refusal even for `principal_ab`.
    let scope_err = get_queried_object(
        host,
        &scope_query(OTHER_ORG),
        Some("principal_ab"),
        DOSSIER,
        "D1",
        &CompartmentedAccess,
    )
    .expect_err("wrong org scope must refuse");
    assert_eq!(scope_err.status, http::StatusCode::FORBIDDEN);

    // Absent-vs-invisible target indistinguishability for `no_grant` via Cedar `norn.read`.
    let absent = get_object(
        host,
        &scope_query(ORG),
        Some("no_grant"),
        DOSSIER,
        "D-ABSENT",
        &CompartmentedAccess,
    )
    .expect_err("absent");
    let invisible = get_object(
        host,
        &scope_query(ORG),
        Some("no_grant"),
        DOSSIER,
        "D-HIDDEN",
        &CompartmentedAccess,
    )
    .expect_err("invisible");
    assert_eq!(absent.status, invisible.status);
    assert_eq!(absent.code, invisible.code);
    assert_eq!(absent.message, invisible.message);
}

#[test]
fn compartmented_four_principal_matrix_enforces_need_to_know_and_noninterference() {
    let fixture = load_pack_fixture();
    let (_dir, path, model) = open_cell_and_model(&fixture);
    let policy = CompartmentedPolicy {
        pdp: Arc::clone(&fixture.pdp),
    };

    {
        let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
        let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> =
            SqliteCell::new(store, &physical(ORG).key(), |cmd: &Command<JsonObject>| {
                cmd.payload.clone()
            })
            .expect("cell");
        let env = ActionEnv {
            model: &model,
            fixture: &fixture,
            policy: &policy,
        };
        let reg = &fixture.actions["intel.dossier.register"];

        for unauthorized in ["no_grant", "principal_a", "principal_b"] {
            let err = run_action(
                &mut cell,
                &env,
                reg,
                unauthorized,
                &format!("deny-{unauthorized}"),
                json!({
                    "id": "D-DENIED",
                    "code": "X-0",
                    "alpha_brief": "alpha-secret",
                    "beta_brief": "beta-secret",
                    "joint_brief": "joint-secret"
                }),
            )
            .expect_err("Cedar PDP must deny unauthorized principal");
            assert!(
                matches!(*err, ActionRefusal::PermissionDenied { .. }),
                "{err:?}"
            );
        }

        seed_active_dossier(&mut cell, &env, "D1", "C-1");
        seed_active_dossier(&mut cell, &env, "D-HIDDEN", RESTRICTED_CODE);
    }

    {
        let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopen");
        let host = DevHost::from_store(store, physical(ORG), |cmd: &Command<JsonObject>| {
            cmd.payload.clone()
        })
        .expect("dev host")
        .with_authorizer(policy)
        .with_pseudonym_secret(Some(&SECRET));

        assert_matrix_point_reads_and_reveals(&host);
        assert_host_scope_and_indistinguishability(&host);
    }

    // Verify exact `marking.revealed` ledger entries: 5 allowed reveals, 0 denied reveals.
    let reveals = read_reveal_ledger_events(&path, &model);
    assert_eq!(
        reveals,
        vec![
            ("principal_a".into(), "D1".into(), "alpha_brief".into()),
            ("principal_b".into(), "D1".into(), "beta_brief".into()),
            ("principal_ab".into(), "D1".into(), "alpha_brief".into()),
            ("principal_ab".into(), "D1".into(), "beta_brief".into()),
            ("principal_ab".into(), "D1".into(), "joint_brief".into()),
        ]
    );
}

fn dispatch_input_for(id: &str, dossier_id: &str, summary: &str) -> Json {
    json!({
        "id": id,
        "dossier_id": dossier_id,
        "summary": summary,
        "alpha_note": null,
        "beta_note": null,
        "joint_note": null
    })
}

fn assert_dispatch_reveal_conjunction(
    host: &DevHost<norn_cell::engine::NativeEngine>,
    path: &Path,
    model: &Model,
) {
    let ok_doc = get_queried_object(
        host,
        &scope_query(ORG),
        Some("no_grant"),
        DISPATCH,
        "DISP-OK-no_grant",
        &CompartmentedAccess,
    )
    .expect("read DISP-OK-no_grant");
    assert_eq!(
        ok_doc.values.as_ref().and_then(|v| v.get("summary")),
        Some(&V::from("public status confirmed"))
    );

    for denied_actor in ["no_grant", "principal_a", "principal_b"] {
        let before_head = host.verified_head().expect("head before").seq;
        let req = NornRevealRequest {
            property: "summary".into(),
            purpose: Some("conjunction check".into()),
            idempotency_key: format!("joint-{denied_actor}"),
        };
        let err = reveal(
            host,
            &scope_query(ORG),
            Some(denied_actor),
            (DISPATCH, "DISP-JOINT"),
            &req,
            &CompartmentedAccess,
        )
        .expect_err("single-compartment principal must NOT reveal A+B tainted summary");
        assert_eq!(err.status, http::StatusCode::FORBIDDEN);
        assert_eq!(host.verified_head().expect("head after").seq, before_head);
    }
    let before_head = host.verified_head().expect("head before").seq;
    let ab_reveal = reveal(
        host,
        &scope_query(ORG),
        Some("principal_ab"),
        (DISPATCH, "DISP-JOINT"),
        &NornRevealRequest {
            property: "summary".into(),
            purpose: Some("conjunction check".into()),
            idempotency_key: "joint-principal_ab".into(),
        },
        &CompartmentedAccess,
    )
    .expect("A+B principal reveals A+B tainted summary");
    assert_eq!(ab_reveal.value, V::from("derived from both alpha and beta"));
    assert_eq!(
        host.verified_head().expect("head after").seq,
        before_head.saturating_add(1)
    );

    // Confirm ledger recorded only `principal_ab`'s conjunction reveal.
    let reveals = read_reveal_ledger_events(path, model);
    assert_eq!(
        reveals,
        vec![("principal_ab".into(), "DISP-JOINT".into(), "summary".into())]
    );
}

fn assert_action_ref_refusals(
    cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    env: &ActionEnv<'_>,
) {
    let before = cell.store().read().dump().expect("dump before");
    let absent_err = run_action(
        cell,
        env,
        &env.fixture.actions["intel.dispatch.create_unmarked"],
        "no_grant",
        "disp-absent",
        dispatch_input_for("DISP-ABSENT", "D-ABSENT", "absent"),
    )
    .expect_err("absent slice target must refuse");
    let invisible_err = run_action(
        cell,
        env,
        &env.fixture.actions["intel.dispatch.create_unmarked"],
        "no_grant",
        "disp-hidden",
        dispatch_input_for("DISP-HIDDEN", "D-HIDDEN", "hidden"),
    )
    .expect_err("Cedar-unreadable slice target must refuse");
    assert_eq!(format!("{absent_err:?}"), format!("{invisible_err:?}"));

    let scope_err = run_action_in_org(
        cell,
        env,
        &env.fixture.actions["intel.dispatch.create_unmarked"],
        OTHER_ORG,
        "principal_ab",
        "disp-wrong-org",
        dispatch_input_for("DISP-WRONG-ORG", "D1", "wrong scope"),
    )
    .expect_err("scope mismatch on action call must refuse");
    // Out-of-scope action call fails Cedar's slice-read gate and conceals existence.
    assert!(
        matches!(*scope_err, ActionRefusal::TargetNotFound),
        "expected fail-closed TargetNotFound on wrong org scope, got {scope_err:?}"
    );

    for denied_actor in ["no_grant", "principal_b"] {
        let err = run_action(
            cell,
            env,
            &env.fixture.actions["intel.dispatch.create_alpha_influenced"],
            denied_actor,
            &format!("disp-alpha-deny-{denied_actor}"),
            dispatch_input_for("DISP-LEAK", "D1", "unauthorized alpha action"),
        )
        .expect_err("Cedar must deny non-alpha actor");
        assert!(
            matches!(*err, ActionRefusal::PermissionDenied { .. }),
            "{err:?}"
        );
    }
    let err = run_action(
        cell,
        env,
        &env.fixture.actions["intel.dispatch.create_alpha_influenced"],
        "principal_a",
        "disp-leak-1",
        dispatch_input_for("DISP-LEAK", "D1", "leaked via alpha predicate"),
    )
    .expect_err("marked predicate influence on createLinks must refuse");
    assert!(
        matches!(
            *err,
            ActionRefusal::Decision(DecisionRefusal::Host(
                CellError::MarkingDeclassificationInvalid(_)
            ))
        ),
        "expected MarkingDeclassificationInvalid, got {err:?}"
    );
    assert_eq!(cell.store().read().dump().expect("dump after"), before);
}

#[test]
fn compartmented_action_references_enforce_positive_witness_marked_influence_and_atomic_refusal() {
    let fixture = load_pack_fixture();
    let (_dir, path, model) = open_cell_and_model(&fixture);
    let policy = CompartmentedPolicy {
        pdp: Arc::clone(&fixture.pdp),
    };

    {
        let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
        let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> =
            SqliteCell::new(store, &physical(ORG).key(), |cmd: &Command<JsonObject>| {
                cmd.payload.clone()
            })
            .expect("cell");
        let env = ActionEnv {
            model: &model,
            fixture: &fixture,
            policy: &policy,
        };
        // Narrow positive witness uses newly registered (draft v1) dossiers so `code`
        // and occurrence lineage remain unmarked while brief properties stay compartmented;
        // `fire_transition` currently reads whole-row lineage on types with marked columns.
        register_dossier(&mut cell, &env, "D1", "C-1");
        register_dossier(&mut cell, &env, "D-HIDDEN", RESTRICTED_CODE);

        // 1. Positive allow witness across all 4 principals + persisted link verification.
        for actor in ["no_grant", "principal_a", "principal_b", "principal_ab"] {
            let disp_id = format!("DISP-OK-{actor}");
            run_action(
                &mut cell,
                &env,
                &fixture.actions["intel.dispatch.create_unmarked"],
                actor,
                &format!("disp-ok-{actor}"),
                dispatch_input_for(&disp_id, "D1", "public status confirmed"),
            )
            .expect("unmarked ACTION-REF-IFC-1 createLinks must commit cleanly");
            let links = cell
                .store()
                .read()
                .links(MAIN_WORLD, DISPATCH, "dossier", &disp_id)
                .expect("read committed dossier link");
            assert_eq!(links.len(), 1, "expected 1 persisted dossier link");
            assert_eq!(links[0].target, "D1");
        }

        // 2 & 3. Absent-vs-unreadable, scope mismatch, Cedar denial, and IFC predicate refusal.
        assert_action_ref_refusals(&mut cell, &env);

        // 4. Conjunction taint propagation (A ∪ B): non-link create reading both alpha & beta
        //    taints `summary` with BOTH compartments.
        run_action(
            &mut cell,
            &env,
            &fixture.actions["intel.dispatch.create_joint_influenced"],
            "principal_ab",
            "disp-joint-1",
            dispatch_input_for("DISP-JOINT", "D1", "derived from both alpha and beta"),
        )
        .expect("joint-tainted non-link create commits");
    }

    let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopen");
    let host = DevHost::from_store(store, physical(ORG), |cmd: &Command<JsonObject>| {
        cmd.payload.clone()
    })
    .expect("host")
    .with_authorizer(policy)
    .with_pseudonym_secret(Some(&SECRET));
    assert_dispatch_reveal_conjunction(&host, &path, &model);
}
