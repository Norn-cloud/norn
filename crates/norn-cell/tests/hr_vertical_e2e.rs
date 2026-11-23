#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "HR vertical end-to-end kernel execution fixtures"
)]
//! End-to-end `norn-cell` execution suite for the HR vertical (`time` + `org` +
//! `hr` + `loc-eg` + `hr-import`): static admission against the Core base,
//! real SQLite cell definition install, leave/roster/attendance/import action
//! and guard execution under compiled Cedar `AccessIR`, and durable PII marking
//! persistence, masking, reveal receipts, and exact-version declassification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::{
    InstalledDefinitionInput, MAIN_WORLD, MarkingDeclassification, MarkingReveal, Model,
    SqliteCell, StoreOptions,
};
use norn_checker::{
    EntityArtifactSetContext, ReceiptSources, admission_receipt_sha256, check_artifact_set,
    check_artifact_set_with_receipt, marking_catalog_from_artifacts,
};
use norn_ir::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactKind, InterfaceDef, MarkingId, MaskPolicy,
    ObjectTypeDef, PropertyTypeDef, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, DenyReason, Pdp,
    PdpActionAuthorizer, PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite, LogicalTime, TypeCatalog, World, commit_action,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE_KEY: &str = "org_cairo_retail/dev";
const ORG_ID: &str = "org_cairo_retail";
const ENV_ID: &str = "dev";
const KNOWN_AT: u64 = 1_795_420_800_000;

const CORE_BASE_FILES: &[&str] = &[
    "core.Agent.interface.json",
    "core.AgentRole.interface.json",
    "core.Resource.interface.json",
    "core.Commitment.interface.json",
    "core.Hold.interface.json",
    "core.EconomicEvent.interface.json",
    "core.LedgerEffect.interface.json",
    "core.Claim.interface.json",
    "norn.SourceEvidence.interface.json",
    "norn.SourceDocument.interface.json",
    "norn.ObservedLinkClaim.entity.json",
    "core.value_types.entity.json",
    "std.value_types.entity.json",
    "core.units.json",
];

const HR_VERTICAL_PACKS: &[&str] = &["time", "org", "hr", "loc-eg", "hr-import"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json_value(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn load_core_base_values() -> Vec<Value> {
    let core_dir = repo_root().join("registry/core/artifacts");
    CORE_BASE_FILES
        .iter()
        .map(|name| read_json_value(&core_dir.join(name)))
        .collect()
}

fn load_hr_vertical_values() -> Vec<Value> {
    let packs_root = repo_root().join("registry/packs");
    let mut paths = Vec::new();
    for pack in HR_VERTICAL_PACKS {
        let dir = packs_root.join(pack).join("artifacts");
        let mut pack_paths: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
            .map(|entry| entry.expect("artifact dir entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        pack_paths.sort();
        paths.extend(pack_paths);
    }
    paths.iter().map(|path| read_json_value(path)).collect()
}

fn decode_envelopes(values: &[Value]) -> Vec<ArtifactEnvelope> {
    values
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("artifact envelope decodes"))
        .collect()
}

fn action_call_type() -> ObjectTypeDef {
    let required = ["actionId", "actor", "turn", "transition", "from", "to"];
    let nullable_text = [
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "ruleBinding",
    ];
    let nullable_json = [
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "rowOperations",
    ];
    let mut fields: Vec<Value> = required
        .iter()
        .map(|id| json!({"id": id, "type": {"kind": "text"}}))
        .collect();
    fields.extend(
        nullable_text.iter().map(
            |id| json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}}),
        ),
    );
    fields.extend(nullable_json.iter().map(
        |id| json!({"id": id, "type": {"kind": "json-ext"}, "constraints": {"nullable": true}}),
    ));
    serde_json::from_value(json!({"name": "norn.ActionCall", "properties": fields}))
        .expect("norn.ActionCall schema decodes")
}

struct RegisteredAction {
    root: ActionRoot,
    artifact_id: String,
    version: String,
}

struct HrPrincipals {
    pdp: Pdp,
}

fn test_scope_key() -> ScopeKey {
    ScopeKey {
        org: ORG_ID.to_owned(),
        tenant: None,
        workspace: None,
        environment: Some(ENV_ID.to_owned()),
        business_unit: None,
    }
}

fn test_scope_map() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), ORG_ID.to_owned()),
        ("environmentId".to_owned(), ENV_ID.to_owned()),
    ])
}

fn actor_roles(actor: &str) -> Vec<String> {
    match actor {
        "principal:employee" => vec!["employee".to_owned()],
        "principal:branch_manager" => vec!["branch_manager".to_owned()],
        "principal:hr_manager" => vec!["hr_manager".to_owned()],
        "principal:hr_admin" => vec!["hr_admin".to_owned()],
        "principal:hr_solver_worker" => vec!["hr_solver_worker".to_owned()],
        "principal:hr_import_worker" => vec!["hr_import_worker".to_owned()],
        _ => Vec::new(),
    }
}

impl HrPrincipals {
    fn new(
        types: &[ObjectTypeDef],
        interfaces: &[InterfaceDef],
        properties: &[PropertyTypeDef],
        actions: &BTreeMap<String, RegisteredAction>,
        policies: &[AccessIr],
    ) -> Self {
        let action_roots: Vec<ActionRoot> = actions.values().map(|a| a.root.clone()).collect();
        let projection = norn_projections::project_cedar_schema_with_catalog(
            types,
            interfaces,
            properties,
            &action_roots,
        );
        let host_schema = norn_projections::project_host_access_schema(types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
                .expect("HR Cedar schema loads");
        let compiled = authz::compile_all_projected(&schema, policies, &projection)
            .expect("HR AccessIR policies compile against projected Cedar schema");

        // Grant every test principal broad action + reveal capabilities so that
        // the compiled Cedar AccessIR policies themselves decide allow vs deny.
        let mut all_capabilities: Vec<String> =
            action_roots.iter().map(|a| a.action_id.clone()).collect();
        all_capabilities.push("norn.read".to_owned());
        all_capabilities.push("norn.reveal".to_owned());

        let grants: Vec<CapabilityGrant> = [
            "principal:employee",
            "principal:branch_manager",
            "principal:hr_manager",
            "principal:hr_admin",
            "principal:hr_solver_worker",
            "principal:hr_import_worker",
        ]
        .into_iter()
        .map(|subject| CapabilityGrant {
            subject_id: subject.to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(test_scope_key()),
            capabilities: all_capabilities.clone(),
            binding: None,
        })
        .collect();

        Self {
            pdp: Pdp::new(compiled, &grants).expect("HR PDP constructs"),
        }
    }

    fn access_request(
        actor: &str,
        action: &str,
        target: &FactKey,
        dimensions: &BTreeMap<String, String>,
    ) -> Result<AccessRequest, ActionDenial> {
        let request_scope = ScopeKey {
            org: dimensions
                .get("orgId")
                .ok_or(ActionDenial::Invisible)?
                .clone(),
            tenant: dimensions.get("tenantId").cloned(),
            workspace: dimensions.get("workspaceId").cloned(),
            environment: dimensions.get("environmentId").cloned(),
            business_unit: dimensions.get("businessUnitId").cloned(),
        };
        Ok(AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.to_owned(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: actor_roles(actor),
                attributes: BTreeMap::new(),
            },
            scope: Some(request_scope.clone()),
            capability: action.to_owned(),
            action_id: action.to_owned(),
            resource: ResourceInput {
                object_type: target.object_type.as_str().to_owned(),
                id: target.key.clone(),
                scope: request_scope,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }

    fn decide_reveal(&self, actor: &str, marking_id: &str) -> PdpDecision {
        let base_request = Self::access_request(
            actor,
            "norn.reveal",
            &FactKey::new("norn.Marking", marking_id),
            &test_scope_map(),
        )
        .expect("reveal request builds");
        self.pdp
            .decide_marking(&base_request, marking_id, "norn.reveal")
    }
}

impl ActionAuthorizer for HrPrincipals {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        PdpActionAuthorizer::new(self.pdp.clone(), |req: &ActionAuthzRequest<'_>| {
            Self::access_request(req.actor, &req.action.action_id, req.target, req.scope)
        })
        .authorize(request)
    }

    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if actor_roles(request.actor).is_empty()
            || request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
            || request.scope.get("environmentId").map(String::as_str) != Some(ENV_ID)
        {
            return Err(ActionDenial::Invisible);
        }
        Ok(())
    }

    fn authorize_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if actor_roles(request.actor).is_empty()
            || request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
            || request.scope.get("environmentId").map(String::as_str) != Some(ENV_ID)
        {
            return Err(ActionDenial::Invisible);
        }
        Ok(())
    }
}

struct HrVerticalFixture {
    base_values: Vec<Value>,
    artifact_values: Vec<Value>,
    artifacts: Vec<ArtifactEnvelope>,
    admission_receipt: String,
    receipt_sha256: String,
    types: Vec<ObjectTypeDef>,
    properties: Vec<PropertyTypeDef>,
    interfaces: Vec<InterfaceDef>,
    actions: BTreeMap<String, RegisteredAction>,
    laws: Vec<ArtifactEnvelope>,
    principals: HrPrincipals,
}

impl HrVerticalFixture {
    fn load() -> Self {
        let base_values = load_core_base_values();
        let artifact_values = load_hr_vertical_values();
        let base = decode_envelopes(&base_values);
        let artifacts = decode_envelopes(&artifact_values);

        let to_runtime = |values: &[Value]| -> Vec<RuntimeValue> {
            values
                .iter()
                .map(|value| RuntimeValue::from_json(value.clone()).expect("exact artifact JSON"))
                .collect()
        };
        let sources = to_runtime(&artifact_values);
        let base_sources = to_runtime(&base_values);
        let registry = BTreeSet::new();
        let checked = check_artifact_set_with_receipt(
            &ReceiptSources::new(&sources, &base_sources).expect("receipt sources"),
            &EntityArtifactSetContext {
                registry: &registry,
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("HR vertical admission receipt");
        assert!(
            checked.receipt.admitted,
            "HR vertical packs must admit with zero error diagnostics: {:#?}",
            checked.diagnostics
        );
        let admission_receipt = String::from_utf8(
            norn_ir::canonical::serialize(
                &checked
                    .receipt
                    .to_canonical_value()
                    .expect("admission receipt canonical value"),
            )
            .expect("admission receipt canonicalizes"),
        )
        .expect("admission receipt is UTF-8");
        let receipt_sha256 =
            admission_receipt_sha256(&admission_receipt).expect("admission receipt sha256");

        let mut types = Vec::new();
        let mut properties = Vec::new();
        let mut interfaces = Vec::new();
        let mut actions = BTreeMap::new();
        let mut policies = Vec::new();

        for document in base_values.iter().chain(artifact_values.iter()) {
            let payload = &document["payload"];
            if let Some(definition) = payload.get("objectType") {
                types.push(serde_json::from_value(definition.clone()).expect("objectType decodes"));
            }
            if let Some(definition) = payload.get("interface") {
                interfaces
                    .push(serde_json::from_value(definition.clone()).expect("interface decodes"));
            }
            if let Some(Value::Array(definitions)) = payload.get("propertyTypes") {
                for definition in definitions {
                    properties.push(
                        serde_json::from_value(definition.clone()).expect("propertyType decodes"),
                    );
                }
            }
            if document["kind"] == "action" {
                let root: ActionRoot =
                    serde_json::from_value(payload.clone()).expect("action root decodes");
                let artifact_id = document["artifactId"]
                    .as_str()
                    .expect("action artifactId")
                    .to_owned();
                let version = document["version"]
                    .as_str()
                    .expect("action version")
                    .to_owned();
                actions.insert(
                    root.action_id.clone(),
                    RegisteredAction {
                        root,
                        artifact_id,
                        version,
                    },
                );
            }
            if document["kind"] == "policy" {
                let policy: AccessIr =
                    serde_json::from_value(payload.clone()).expect("policy root decodes");
                policies.push(policy);
            }
        }
        types.push(action_call_type());

        let laws: Vec<ArtifactEnvelope> = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Law)
            .cloned()
            .collect();
        let principals = HrPrincipals::new(&types, &interfaces, &properties, &actions, &policies);

        Self {
            base_values,
            artifact_values,
            artifacts,
            admission_receipt,
            receipt_sha256,
            types,
            properties,
            interfaces,
            actions,
            laws,
            principals,
        }
    }

    fn model(&self) -> Model {
        Model::from_definitions_with_interfaces(&self.types, &self.properties, &self.interfaces)
            .expect("HR vertical model projects")
            .with_governing_laws(&self.laws)
            .expect("HR vertical governing laws bind")
    }

    fn catalog(&self) -> TypeCatalog<'_> {
        TypeCatalog::new(&self.types)
            .with_property_types(&self.properties)
            .with_interfaces(&self.interfaces)
            .with_governing_laws(&self.laws)
    }

    fn install_input(&self) -> InstalledDefinitionInput {
        let mut combined = self.artifact_values.clone();
        combined.extend(self.base_values.iter().cloned());
        let canonical_set = String::from_utf8(
            RuntimeValue::from_json(Value::Array(combined))
                .expect("HR vertical + core base artifact set is exact JSON")
                .canonical_json()
                .expect("HR vertical + core base artifact set canonicalizes"),
        )
        .expect("canonical HR vertical + core base set is UTF-8");
        InstalledDefinitionInput {
            canonical_set,
            registry: Vec::new(),
            receipt_sha256: self.receipt_sha256.clone(),
            admission_receipt: Some(self.admission_receipt.clone()),
            readmission: None,
            diff: json!({"added": {"count": self.artifacts.len()}}),
            installed_at: KNOWN_AT,
            layout: None,
        }
    }

    fn open_installed_cell(&self, path: &Path) -> Cell {
        let model = self.model();
        let mut cell = Cell::open(
            path,
            model.clone(),
            StoreOptions::default(),
            SCOPE_KEY,
            |_: &Command<JsonObject>| panic!("HR vertical E2E actions are synchronous"),
        )
        .expect("SQLite HR cell opens");
        cell.advance_to(LogicalTime::from_millis(KNOWN_AT));
        cell.install_definitions(model, self.install_input(), Some(0))
            .expect("admitted HR vertical definitions install");
        cell
    }

    fn invoke(
        &self,
        cell: &mut Cell,
        action_id: &str,
        input: &Value,
        actor: &str,
        idempotency_key: &str,
    ) -> Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>> {
        let registered = &self.actions[action_id];
        let request = ActionCall {
            action: &registered.root,
            action_artifact_id: &registered.artifact_id,
            action_version: &registered.version,
            scope: test_scope_map(),
            actor: actor.to_owned(),
            input: decode_json_object(input),
            idempotency_key: idempotency_key.to_owned(),
            object_type: None,
            turn: DecisionTurn {
                id: format!("turn:{idempotency_key}"),
                now: LogicalTime::from_millis(KNOWN_AT),
                world: World::Main,
                consumes: None,
            },
        };
        commit_action(cell, &self.catalog(), &request, &self.principals).map_err(Box::new)
    }
}

fn decode_json_object(value: &Value) -> JsonObject {
    serde_json::from_value(value.clone()).expect("JSON object decodes")
}

fn put_fact(object_type: &str, id: &str, values: &Value) -> FactWrite {
    FactWrite {
        key: FactKey::new(object_type, id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_value(values.clone()).expect("fact values decode"),
        },
    }
}

fn add_link(from_type: &str, from_id: &str, link: &str, to_type: &str, to_id: &str) -> LinkWrite {
    LinkWrite {
        key: LinkKey::new(
            FactKey::new(from_type, from_id),
            link,
            FactKey::new(to_type, to_id),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    }
}

fn status_of(cell: &Cell, object_type: &str, id: &str) -> String {
    cell.load(&FactKey::new(object_type, id))
        .expect("object loads")
        .values
        .expect("object exists")["status"]
        .as_str()
        .expect("status is text")
        .to_owned()
}

fn mask_text(value: &str, policy: &MaskPolicy) -> Option<String> {
    match policy {
        MaskPolicy::Redacted {} => Some("[REDACTED]".to_owned()),
        MaskPolicy::LastN { count } => {
            let chars: Vec<char> = value.chars().collect();
            let keep = usize::try_from(*count).unwrap_or(usize::MAX);
            let tail: String = if chars.len() > keep {
                chars
                    .get(chars.len().saturating_sub(keep)..)
                    .unwrap_or_default()
                    .iter()
                    .collect()
            } else {
                String::new()
            };
            Some(format!("[REDACTED]{tail}"))
        }
        MaskPolicy::Null {} | MaskPolicy::Refuse {} => None,
        MaskPolicy::Pseudonym { subject } => Some(format!("{subject} #PSEUDONYM")),
    }
}

#[test]
fn hr_vertical_packs_admit_together_against_core_base() {
    let base = decode_envelopes(&load_core_base_values());
    let artifacts = decode_envelopes(&load_hr_vertical_values());

    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    let errors: Vec<_> = findings.iter().filter(|d| d.is_error()).collect();
    assert!(
        errors.is_empty(),
        "time + org + hr + loc-eg + hr-import must admit cleanly: {errors:#?}"
    );

    let law_ids: BTreeSet<&str> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    assert!(
        law_ids.contains("hr.LeaveBalance.non_negative.law")
            && law_ids.contains("hr.Roster.published_requires_receipt.law")
            && law_ids.contains("loc_eg.LaborProfile.statutory_floors.law")
            && law_ids.contains("hr_import.ExtractedCandidate.confidence_bounds.law")
            && law_ids.contains("hr_import.ExtractedCandidate.promoted_requires_clean_review.law"),
        "expected HR, loc-eg, and hr-import laws in admitted set: {law_ids:?}"
    );
    let law_findings: Vec<_> = findings
        .iter()
        .filter(|d| {
            d.artifact_id
                .as_deref()
                .is_some_and(|id| law_ids.contains(id))
        })
        .collect();
    assert!(
        law_findings.is_empty(),
        "no checker findings against HR vertical laws: {law_findings:#?}"
    );

    let catalog = marking_catalog_from_artifacts(&artifacts);
    assert_eq!(
        catalog
            .get(&MarkingId("pii.gov_id".to_owned()))
            .expect("pii.gov_id declared")
            .mask,
        MaskPolicy::LastN { count: 4 }
    );
    assert_eq!(
        catalog
            .get(&MarkingId("pii.raw_chat_media".to_owned()))
            .expect("pii.raw_chat_media declared")
            .mask,
        MaskPolicy::Redacted {}
    );
}

#[test]
fn hr_leave_and_roster_lifecycles_and_guards_enforce_in_real_cell() {
    let fixture = HrVerticalFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hr-vertical.sqlite");
    let mut cell = fixture.open_installed_cell(&path);

    let mut seed = CellCommit::empty();
    seed.writes.extend([
        put_fact(
            "time.Calendar",
            "cal_cairo",
            &json!({
                "id": "cal_cairo",
                "code": "eg_cairo",
                "label": "Cairo Retail Calendar",
                "iana_timezone": "Africa/Cairo",
                "dst_policy": "reject",
                "tzdb_version": "2026a",
                "workweek_start": "sunday"
            }),
        ),
        put_fact(
            "org.OrgUnit",
            "unit_cairo_nc",
            &json!({
                "id": "unit_cairo_nc",
                "display_name": "Nasr City Branch",
                "kind": "organization",
                "unit_code": "CAIRO-NC-01",
                "business_unit_code": "EG-RETAIL"
            }),
        ),
        put_fact(
            "hr.Person",
            "person_nour",
            &json!({
                "id": "person_nour",
                "display_name": "Nour Hassan",
                "kind": "person",
                "gender": "female",
                "national_id": "29805150101234",
                "phone": "+201000000001",
                "primary_skill": "cashier"
            }),
        ),
        put_fact(
            "hr.LeaveBalance",
            "bal_nour_2026",
            &json!({
                "id": "bal_nour_2026",
                "resource_code": "annual_leave",
                "quantity": 5,
                "unit": "d",
                "currency": null,
                "valuation_method": null,
                "leave_kind": "annual",
                "period_year": 2026
            }),
        ),
        put_fact(
            "hr.LeaveRequest",
            "lr_valid",
            &json!({
                "id": "lr_valid",
                "leave_kind": "annual",
                "starts_on": "2026-12-01",
                "ends_on": "2026-12-02",
                "days": 2,
                "medical_note": null,
                "status": "draft"
            }),
        ),
        put_fact(
            "hr.LeaveRequest",
            "lr_over",
            &json!({
                "id": "lr_over",
                "leave_kind": "annual",
                "starts_on": "2026-12-05",
                "ends_on": "2026-12-14",
                "days": 10,
                "medical_note": null,
                "status": "draft"
            }),
        ),
        put_fact(
            "hr.LeaveRequest",
            "lr_zero",
            &json!({
                "id": "lr_zero",
                "leave_kind": "annual",
                "starts_on": "2026-12-15",
                "ends_on": "2026-12-15",
                "days": 0,
                "medical_note": null,
                "status": "draft"
            }),
        ),
        put_fact(
            "hr.PreferenceProfile",
            "pref_cairo",
            &json!({
                "id": "pref_cairo",
                "label": "Cairo Retail Balanced",
                "currency": "EGP",
                "weekly_overtime_budget": 1500,
                "min_rest_hours": 11,
                "max_weekly_hours": 48,
                "max_consecutive_days": 6,
                "weekend_fairness_priority": "high",
                "staff_preference_priority": "medium",
                "avoid_split_shifts": true
            }),
        ),
        put_fact(
            "hr.Roster",
            "roster_no_receipt",
            &json!({
                "id": "roster_no_receipt",
                "period_start": "2026-11-23",
                "period_end": "2026-11-29",
                "solver_receipt_digest": null,
                "status": "draft"
            }),
        ),
        put_fact(
            "hr.Roster",
            "roster_with_receipt",
            &json!({
                "id": "roster_with_receipt",
                "period_start": "2026-11-30",
                "period_end": "2026-12-06",
                "solver_receipt_digest": "sha256:solver_receipt_cairo_w49",
                "status": "draft"
            }),
        ),
        put_fact(
            "hr.ShiftCommitment",
            "shift_nour_mon",
            &json!({
                "id": "shift_nour_mon",
                "status": "promised",
                "due_at": "2026-11-23T17:00:00Z",
                "quantity": 8,
                "unit": "h",
                "currency": null,
                "starts_at": "2026-11-23T09:00:00Z",
                "ends_at": "2026-11-23T17:00:00Z",
                "role_code": "cashier"
            }),
        ),
        put_fact(
            "hr_import.SourceBatch",
            "batch_cairo_1",
            &json!({
                "id": "batch_cairo_1",
                "channel": "spreadsheet_csv",
                "blob_ref": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "raw_excerpt": "CAIRO-NC-01,EMP-SYN-003,Salma Tarek,2026-11-22,14:05,,missing_clock_out",
                "captured_at": "2026-11-22T20:30:00Z",
                "status": "reviewing"
            }),
        ),
        put_fact(
            "hr_import.ExtractedCandidate",
            "cand_dirty",
            &json!({
                "id": "cand_dirty",
                "target_kind": "attendance_event",
                "confidence_bps": 6200,
                "source_span": "attendance-sheet-sample.csv:row:4",
                "extracted_at": "2026-11-23T08:05:00Z",
                "observed_at": "2026-11-22T20:30:00Z",
                "effective_on": "2026-11-22",
                "open_error_count": 1,
                "review_receipt_digest": null,
                "status": "staged"
            }),
        ),
        put_fact(
            "hr_import.ExtractedCandidate",
            "cand_clean",
            &json!({
                "id": "cand_clean",
                "target_kind": "shift_commitment",
                "confidence_bps": 9450,
                "source_span": "whatsapp-cairo-shifts.txt:line:4-5",
                "extracted_at": "2026-11-23T08:00:00Z",
                "observed_at": "2026-11-22T16:10:00Z",
                "effective_on": "2026-11-25",
                "open_error_count": 0,
                "review_receipt_digest": "sha256:review_receipt_001",
                "status": "staged"
            }),
        ),
    ]);
    seed.links.extend([
        add_link(
            "hr.LeaveBalance",
            "bal_nour_2026",
            "owner",
            "hr.Person",
            "person_nour",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_valid",
            "person",
            "hr.Person",
            "person_nour",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_valid",
            "balance",
            "hr.LeaveBalance",
            "bal_nour_2026",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_over",
            "person",
            "hr.Person",
            "person_nour",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_over",
            "balance",
            "hr.LeaveBalance",
            "bal_nour_2026",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_zero",
            "person",
            "hr.Person",
            "person_nour",
        ),
        add_link(
            "hr.LeaveRequest",
            "lr_zero",
            "balance",
            "hr.LeaveBalance",
            "bal_nour_2026",
        ),
        add_link(
            "hr.PreferenceProfile",
            "pref_cairo",
            "unit",
            "org.OrgUnit",
            "unit_cairo_nc",
        ),
        add_link(
            "hr.PreferenceProfile",
            "pref_cairo",
            "calendar",
            "time.Calendar",
            "cal_cairo",
        ),
        add_link(
            "hr.Roster",
            "roster_no_receipt",
            "unit",
            "org.OrgUnit",
            "unit_cairo_nc",
        ),
        add_link(
            "hr.Roster",
            "roster_no_receipt",
            "preference_profile",
            "hr.PreferenceProfile",
            "pref_cairo",
        ),
        add_link(
            "hr.Roster",
            "roster_with_receipt",
            "unit",
            "org.OrgUnit",
            "unit_cairo_nc",
        ),
        add_link(
            "hr.Roster",
            "roster_with_receipt",
            "preference_profile",
            "hr.PreferenceProfile",
            "pref_cairo",
        ),
        add_link(
            "hr.ShiftCommitment",
            "shift_nour_mon",
            "provider",
            "hr.Person",
            "person_nour",
        ),
        add_link(
            "hr.ShiftCommitment",
            "shift_nour_mon",
            "receiver",
            "org.OrgUnit",
            "unit_cairo_nc",
        ),
        add_link(
            "hr.ShiftCommitment",
            "shift_nour_mon",
            "roster",
            "hr.Roster",
            "roster_with_receipt",
        ),
        add_link(
            "hr_import.SourceBatch",
            "batch_cairo_1",
            "unit",
            "org.OrgUnit",
            "unit_cairo_nc",
        ),
        add_link(
            "hr_import.ExtractedCandidate",
            "cand_dirty",
            "batch",
            "hr_import.SourceBatch",
            "batch_cairo_1",
        ),
        add_link(
            "hr_import.ExtractedCandidate",
            "cand_clean",
            "batch",
            "hr_import.SourceBatch",
            "batch_cairo_1",
        ),
    ]);
    cell.commit(seed)
        .expect("seed HR vertical objects and links");

    // 1. Leave submit -> approve succeeds; employee self-approval is denied by AccessIR;
    //    over-balance (10 > 5) and zero-day (0) leave requests are refused without mutation.
    fixture
        .invoke(
            &mut cell,
            "hr.leave.submit",
            &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_nour_2026"}),
            "principal:employee",
            "leave-submit-valid",
        )
        .expect("valid 2-day leave request submits");
    assert_eq!(status_of(&cell, "hr.LeaveRequest", "lr_valid"), "pending");

    let self_approve = fixture
        .invoke(
            &mut cell,
            "hr.leave.approve",
            &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_nour_2026"}),
            "principal:employee",
            "leave-self-approve-denied",
        )
        .expect_err("employee cannot self-approve leave");
    assert_eq!(self_approve.code(), "action.permission_denied");
    assert_eq!(status_of(&cell, "hr.LeaveRequest", "lr_valid"), "pending");

    fixture
        .invoke(
            &mut cell,
            "hr.leave.approve",
            &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_nour_2026"}),
            "principal:branch_manager",
            "leave-approve-valid",
        )
        .expect("branch manager approves valid pending leave");
    assert_eq!(status_of(&cell, "hr.LeaveRequest", "lr_valid"), "approved");

    let before_refusals = cell
        .store()
        .read()
        .dump()
        .expect("snapshot before leave refusals");
    for (req_id, key) in [
        ("lr_over", "leave-submit-over"),
        ("lr_zero", "leave-submit-zero"),
    ] {
        let refusal = fixture
            .invoke(
                &mut cell,
                "hr.leave.submit",
                &json!({"leaveRequestId": req_id, "balanceId": "bal_nour_2026"}),
                "principal:employee",
                key,
            )
            .expect_err("invalid leave request must be refused");
        assert_eq!(refusal.code(), "action.precondition_failed");
        assert_eq!(status_of(&cell, "hr.LeaveRequest", req_id), "draft");
    }
    assert_eq!(
        cell.store()
            .read()
            .dump()
            .expect("snapshot after leave refusals"),
        before_refusals
    );

    // Direct write violating `hr.LeaveBalance.non_negative.law` is refused by the kernel.
    let mut bad_balance = CellCommit::empty();
    bad_balance.writes.push(put_fact(
        "hr.LeaveBalance",
        "bal_negative",
        &json!({
            "id": "bal_negative",
            "resource_code": "annual_leave",
            "quantity": -1,
            "unit": "d",
            "currency": null,
            "valuation_method": null,
            "leave_kind": "annual",
            "period_year": 2026
        }),
    ));
    bad_balance.links.push(add_link(
        "hr.LeaveBalance",
        "bal_negative",
        "owner",
        "hr.Person",
        "person_nour",
    ));
    assert!(
        cell.commit(bad_balance).is_err(),
        "negative leave balance must be refused by hr.LeaveBalance.non_negative.law"
    );

    // 2. Roster propose/publish: publish without solver receipt is refused; with receipt succeeds.
    fixture
        .invoke(
            &mut cell,
            "hr.roster.propose",
            &json!({"rosterId": "roster_no_receipt"}),
            "principal:hr_solver_worker",
            "roster-propose-no-receipt",
        )
        .expect("solver worker proposes draft roster");
    assert_eq!(
        status_of(&cell, "hr.Roster", "roster_no_receipt"),
        "proposed"
    );

    let no_receipt_publish = fixture
        .invoke(
            &mut cell,
            "hr.roster.publish",
            &json!({"rosterId": "roster_no_receipt"}),
            "principal:branch_manager",
            "roster-publish-no-receipt",
        )
        .expect_err("roster without solver_receipt_digest cannot publish");
    assert_eq!(no_receipt_publish.code(), "action.precondition_failed");
    assert_eq!(
        status_of(&cell, "hr.Roster", "roster_no_receipt"),
        "proposed"
    );

    fixture
        .invoke(
            &mut cell,
            "hr.roster.propose",
            &json!({"rosterId": "roster_with_receipt"}),
            "principal:hr_solver_worker",
            "roster-propose-with-receipt",
        )
        .expect("solver worker proposes receipted roster");
    fixture
        .invoke(
            &mut cell,
            "hr.roster.publish",
            &json!({"rosterId": "roster_with_receipt"}),
            "principal:branch_manager",
            "roster-publish-with-receipt",
        )
        .expect("branch manager publishes roster once solver receipt is present");
    assert_eq!(
        status_of(&cell, "hr.Roster", "roster_with_receipt"),
        "published"
    );

    // 3. Immutable attendance create-only (`hr.attendance.record`): first create succeeds,
    //    exact retry replays, and a distinct call attempting to overwrite is refused.
    let attendance_input = json!({
        "id": "att_nour_20261123",
        "action": "produce",
        "occurred_at": "2026-11-23T17:05:00Z",
        "quantity": "8",
        "unit": "h",
        "currency": null,
        "clock_in_at": "2026-11-23T09:02:00Z",
        "clock_out_at": "2026-11-23T17:05:00Z",
        "providerId": "person_nour",
        "receiverId": "unit_cairo_nc",
        "resourceId": "bal_nour_2026",
        "shiftCommitmentId": "shift_nour_mon"
    });
    fixture
        .invoke(
            &mut cell,
            "hr.attendance.record",
            &attendance_input,
            "principal:branch_manager",
            "att-record-1",
        )
        .expect("first create-only attendance event commits");
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new("hr.AttendanceEvent", "att_nour_20261123"),
            "fulfils",
            FactKey::new("hr.ShiftCommitment", "shift_nour_mon"),
        ))
        .expect("link loads")
        .exists()
    );
    let replayed = fixture
        .invoke(
            &mut cell,
            "hr.attendance.record",
            &attendance_input,
            "principal:branch_manager",
            "att-record-1",
        )
        .expect("exact attendance retry replays");
    assert!(matches!(replayed, ActionOutcome::Replayed { .. }));

    let overwrite = fixture
        .invoke(
            &mut cell,
            "hr.attendance.record",
            &attendance_input,
            "principal:branch_manager",
            "att-record-overwrite-attempt",
        )
        .expect_err("posted attendance event cannot be overwritten");
    assert_eq!(overwrite.code(), "action.write_mode_failed");

    // 4. Historical import quarantine & promotion authority/guards (`hr-import`).
    fixture
        .invoke(
            &mut cell,
            "hr_import.candidate.quarantine",
            &json!({"candidateId": "cand_dirty"}),
            "principal:hr_import_worker",
            "import-quarantine-dirty",
        )
        .expect("import worker may quarantine nonconforming candidate");
    assert_eq!(
        status_of(&cell, "hr_import.ExtractedCandidate", "cand_dirty"),
        "quarantined"
    );

    let worker_promote = fixture
        .invoke(
            &mut cell,
            "hr_import.candidate.promote",
            &json!({"candidateId": "cand_clean"}),
            "principal:hr_import_worker",
            "import-worker-promote-denied",
        )
        .expect_err("import worker must never promote candidates");
    assert_eq!(worker_promote.code(), "action.permission_denied");

    let dirty_promote = fixture
        .invoke(
            &mut cell,
            "hr_import.candidate.promote",
            &json!({"candidateId": "cand_dirty"}),
            "principal:hr_manager",
            "import-promote-dirty-refused",
        )
        .expect_err("candidate with open error findings cannot be promoted");
    assert_eq!(dirty_promote.code(), "action.precondition_failed");

    fixture
        .invoke(
            &mut cell,
            "hr_import.candidate.promote",
            &json!({"candidateId": "cand_clean"}),
            "principal:hr_manager",
            "import-promote-clean",
        )
        .expect("HR manager promotes clean reviewed candidate");
    assert_eq!(
        status_of(&cell, "hr_import.ExtractedCandidate", "cand_clean"),
        "promoted"
    );

    // Durable reopen preserves exact state and hash-chained ledger integrity.
    let dump = cell.store().read().dump().expect("final dump");
    let head = cell
        .store()
        .read()
        .verify_ledger()
        .expect("ledger verifies");
    drop(cell);
    let reopened = Cell::open(
        &path,
        fixture.model(),
        StoreOptions::default(),
        SCOPE_KEY,
        |_: &Command<JsonObject>| panic!("reopen executes no commands"),
    )
    .expect("reopened HR cell");
    assert_eq!(reopened.store().read().dump().expect("reopened dump"), dump);
    assert_eq!(
        reopened
            .store()
            .read()
            .verify_ledger()
            .expect("reopened ledger"),
        head
    );
}

#[test]
fn hr_pii_markings_persist_and_mask_in_real_cell() {
    let fixture = HrVerticalFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hr-markings.sqlite");
    let mut cell = fixture.open_installed_cell(&path);

    let raw_national_id = "29805150101234";
    let mut seed = CellCommit::empty();
    seed.writes.extend([
        put_fact(
            "org.OrgUnit",
            "unit_cairo_nc",
            &json!({
                "id": "unit_cairo_nc",
                "display_name": "Nasr City Branch",
                "kind": "organization",
                "unit_code": "CAIRO-NC-01",
                "business_unit_code": "EG-RETAIL"
            }),
        ),
        put_fact(
            "hr.Person",
            "person_nour",
            &json!({
                "id": "person_nour",
                "display_name": "Nour Hassan",
                "kind": "person",
                "gender": "female",
                "national_id": raw_national_id,
                "phone": "+201000000001",
                "primary_skill": "cashier"
            }),
        ),
        put_fact(
            "hr_import.SourceBatch",
            "batch_wa_1",
            &json!({
                "id": "batch_wa_1",
                "channel": "whatsapp_export",
                "blob_ref": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "raw_excerpt": "Nour Hassan: Salma Tarek evening cashier 2026-11-25 14:00-22:00 EET.",
                "captured_at": "2026-11-22T16:10:00Z",
                "status": "received"
            }),
        ),
    ]);
    seed.links.push(add_link(
        "hr_import.SourceBatch",
        "batch_wa_1",
        "unit",
        "org.OrgUnit",
        "unit_cairo_nc",
    ));
    cell.commit(seed)
        .expect("commit marked Person and SourceBatch");

    // Cell marking storage records every declared PII marking on the persisted facts.
    let person_markings = cell
        .store()
        .read()
        .effective_markings(MAIN_WORLD, "hr.Person", "person_nour", None)
        .expect("effective markings query")
        .expect("complete marking snapshot");
    assert_eq!(
        person_markings["national_id"],
        vec!["pii.gov_id".to_owned()]
    );
    assert_eq!(
        person_markings["gender"],
        vec!["pii.demographic".to_owned()]
    );
    assert_eq!(person_markings["phone"], vec!["pii.demographic".to_owned()]);
    assert_eq!(person_markings["display_name"], [] as [String; 0]);

    let batch_markings = cell
        .store()
        .read()
        .effective_markings(MAIN_WORLD, "hr_import.SourceBatch", "batch_wa_1", None)
        .expect("batch markings query")
        .expect("complete batch marking snapshot");
    assert_eq!(
        batch_markings["raw_excerpt"],
        vec!["pii.raw_chat_media".to_owned()]
    );

    // Mask policy from `hr.privacy.markings` + fail-closed reveal under `hr.reveal.policy`:
    // `branch_manager` is denied reveal and sees only the `last-n(4)` mask;
    // `hr_admin` is allowed reveal and commits an audited reveal receipt without the raw value.
    let catalog = marking_catalog_from_artifacts(&fixture.artifacts);
    let gov_id_policy = &catalog
        .get(&MarkingId("pii.gov_id".to_owned()))
        .expect("pii.gov_id policy")
        .mask;
    assert_eq!(gov_id_policy, &MaskPolicy::LastN { count: 4 });

    let manager_decision = fixture
        .principals
        .decide_reveal("principal:branch_manager", "pii.gov_id");
    assert_eq!(manager_decision, PdpDecision::Deny(DenyReason::Access));

    let stored_person = cell
        .store()
        .read()
        .object(MAIN_WORLD, "hr.Person", "person_nour")
        .expect("person read")
        .expect("person present");
    let stored_gov_id = stored_person.values["national_id"]
        .as_str()
        .expect("national_id text");
    assert_eq!(stored_gov_id, raw_national_id);
    let masked = mask_text(stored_gov_id, gov_id_policy);
    assert_eq!(masked.as_deref(), Some("[REDACTED]1234"));
    assert_ne!(masked.as_deref(), Some(raw_national_id));

    let admin_decision = fixture
        .principals
        .decide_reveal("principal:hr_admin", "pii.gov_id");
    assert_eq!(admin_decision, PdpDecision::Allow);

    let reveal = MarkingReveal {
        object_type: "hr.Person".to_owned(),
        key: "person_nour".to_owned(),
        property: "national_id".to_owned(),
        version: 1,
        markings: vec!["pii.gov_id".to_owned()],
        actor: "principal:hr_admin".to_owned(),
        purpose: "social insurance enrollment audit".to_owned(),
    };
    cell.record_reveal("reveal-nour-gov-id-1", &reveal)
        .expect("audited reveal receipt commits");

    let ledger_after_reveal = cell.store().read().ledger(0).expect("ledger entries");
    let reveal_entry = ledger_after_reveal
        .iter()
        .find(|entry| {
            entry.event.get("type") == Some(&JsonValue::String("marking.revealed".to_owned()))
        })
        .expect("marking.revealed receipt is persisted in ledger");
    let reveal_json = serde_json::to_string(&reveal_entry.event).expect("event serializes");
    assert!(reveal_json.contains("principal:hr_admin"));
    assert!(reveal_json.contains("social insurance enrollment audit"));
    assert!(
        !reveal_json.contains(raw_national_id),
        "reveal receipt must never carry the raw PII value: {reveal_json}"
    );

    // Exact-version declassification clears `pii.gov_id` on the current snapshot only
    // while retaining the historical classification on version 1.
    cell.declassify_markings(
        "declassify-nour-gov-id-1",
        MarkingDeclassification {
            object_type: "hr.Person".to_owned(),
            key: "person_nour".to_owned(),
            property: "national_id".to_owned(),
            expected_version: 1,
            markings: vec!["pii.gov_id".to_owned()],
            actor: "principal:hr_admin".to_owned(),
            reason: "synthetic test fixture declassification".to_owned(),
        },
    )
    .expect("exact-version declassification commits");

    let current_after = cell
        .store()
        .read()
        .effective_markings(MAIN_WORLD, "hr.Person", "person_nour", None)
        .expect("current markings")
        .expect("complete snapshot");
    let historical_v1 = cell
        .store()
        .read()
        .effective_markings(MAIN_WORLD, "hr.Person", "person_nour", Some(1))
        .expect("historical markings")
        .expect("complete snapshot");
    assert_eq!(current_after["national_id"], [] as [String; 0]);
    assert_eq!(historical_v1["national_id"], vec!["pii.gov_id".to_owned()]);
    cell.store()
        .read()
        .verify_ledger()
        .expect("ledger verifies");
}
