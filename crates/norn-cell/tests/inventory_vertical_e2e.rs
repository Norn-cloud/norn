#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "inventory domain pack end-to-end kernel execution fixtures"
)]
//! End-to-end `norn-cell` execution suite for the Governed Inventory &
//! Warehouse Domain Pack (`registry/packs/inventory/`): static admission
//! against the Core REA + TimeInterval base, real SQLite cell definition
//! install with checker receipt, law and `core.TimeInterval` refusals,
//! operational movement/reservation guards (active caller warehouses plus
//! balance owner/custodian warehouses, `(action, movement_kind)` consistency,
//! unit match, unreserved availability on outflows/consumptions, active
//! bins/lots), action/decision-preparation immutability (`StockMovement`
//! `create_only` + `"recorded"`, `LotBatch` `"recalled"`, `CycleCount`
//! `"approved"`/`"posted"`), and fail-closed Cedar `AccessIR` role and scope
//! enforcement.
//!
//! Boundary notes: `lifecycle.immutable` is enforced on kernel
//! decision-preparation (`FactChange::Set`), not raw `CellHost::commit`
//! writes or `FactChange::Delete`. `inventory.movement.record` and
//! `inventory.reservation.create` are single-object `upsert_input_object`
//! writes; they do not update `StockBalance` in the same commit.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::{InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{
    EntityArtifactSetContext, ReceiptSources, admission_receipt_sha256, check_artifact_set,
    check_artifact_set_with_receipt,
};
use norn_ir::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactKind, DataQualityRuleRef, DecisionOutput,
    FactChange, InterfaceDef, InterfaceName, InvariantOwner, ObjectTypeDef, ObjectTypeName,
    PropertyId, PropertyTypeDef, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::governing_laws::LawEvaluationError;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, CommitError, DecisionRefusal, DecisionTurn, FactEvent, FactKey, FactMutation,
    FactSlice, FactVersion, FactWrite, LinkKey, LinkMutation, LinkWrite, LogicalTime, TypeCatalog,
    World, commit_action, prepare_decision,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE_KEY: &str = "org_global_logistics/dev";
const ORG_ID: &str = "org_global_logistics";
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
    "core.TimeInterval.interface.json",
    "norn.SourceEvidence.interface.json",
    "norn.SourceDocument.interface.json",
    "norn.ObservedLinkClaim.entity.json",
    "core.value_types.entity.json",
    "std.value_types.entity.json",
    "core.units.json",
    "core.time.json",
];

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

fn load_inventory_pack_values() -> Vec<Value> {
    let dir = repo_root().join("registry/packs/inventory/artifacts");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("artifact dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
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

struct ExtractedDefinitions {
    types: Vec<ObjectTypeDef>,
    properties: Vec<PropertyTypeDef>,
    interfaces: Vec<InterfaceDef>,
    actions: BTreeMap<String, RegisteredAction>,
    policies: Vec<AccessIr>,
}

fn extract_definitions(base_values: &[Value], artifact_values: &[Value]) -> ExtractedDefinitions {
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
            interfaces.push(serde_json::from_value(definition.clone()).expect("interface decodes"));
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

    ExtractedDefinitions {
        types,
        properties,
        interfaces,
        actions,
        policies,
    }
}

fn compute_admission_receipt(
    base_values: &[Value],
    artifact_values: &[Value],
    base: &[ArtifactEnvelope],
) -> (String, String) {
    let to_runtime = |values: &[Value]| -> Vec<RuntimeValue> {
        values
            .iter()
            .map(|value| RuntimeValue::from_json(value.clone()).expect("exact artifact JSON"))
            .collect()
    };
    let sources = to_runtime(artifact_values);
    let base_sources = to_runtime(base_values);
    let registry = BTreeSet::new();
    let checked = check_artifact_set_with_receipt(
        &ReceiptSources::new(&sources, &base_sources).expect("receipt sources"),
        &EntityArtifactSetContext {
            registry: &registry,
            base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("inventory pack admission receipt");
    assert!(
        checked.receipt.admitted,
        "inventory pack must admit with zero error diagnostics: {:#?}",
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
    (admission_receipt, receipt_sha256)
}

struct InventoryPrincipals {
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
        "principal:warehouse_clerk" => vec!["warehouse_clerk".to_owned()],
        "principal:quality_inspector" => vec!["quality_inspector".to_owned()],
        "principal:inventory_manager" => vec!["inventory_manager".to_owned()],
        "principal:owner" => vec!["owner".to_owned()],
        _ => Vec::<String>::new(),
    }
}

impl InventoryPrincipals {
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
                .expect("inventory Cedar schema loads");
        let compiled = authz::compile_all_projected(&schema, policies, &projection)
            .expect("inventory AccessIR policies compile against projected Cedar schema");

        // Grant every test principal broad action capabilities scoped strictly
        // to `ORG_ID`/`ENV_ID` so the compiled Cedar AccessIR policy and scope
        // checks decide allow vs deny.
        let mut all_capabilities: Vec<String> =
            action_roots.iter().map(|a| a.action_id.clone()).collect();
        all_capabilities.push("norn.read".to_owned());

        let grants: Vec<CapabilityGrant> = [
            "principal:warehouse_clerk",
            "principal:quality_inspector",
            "principal:inventory_manager",
            "principal:owner",
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
            pdp: Pdp::new(compiled, &grants).expect("inventory PDP constructs"),
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
}

impl ActionAuthorizer for InventoryPrincipals {
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

struct InventoryFixture {
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
    principals: InventoryPrincipals,
}

impl InventoryFixture {
    fn load() -> Self {
        let base_values = load_core_base_values();
        let artifact_values = load_inventory_pack_values();
        let base = decode_envelopes(&base_values);
        let artifacts = decode_envelopes(&artifact_values);
        let (admission_receipt, receipt_sha256) =
            compute_admission_receipt(&base_values, &artifact_values, &base);
        let defs = extract_definitions(&base_values, &artifact_values);
        let laws: Vec<ArtifactEnvelope> = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Law)
            .cloned()
            .collect();
        let principals = InventoryPrincipals::new(
            &defs.types,
            &defs.interfaces,
            &defs.properties,
            &defs.actions,
            &defs.policies,
        );

        Self {
            base_values,
            artifact_values,
            artifacts,
            admission_receipt,
            receipt_sha256,
            types: defs.types,
            properties: defs.properties,
            interfaces: defs.interfaces,
            actions: defs.actions,
            laws,
            principals,
        }
    }

    fn model(&self) -> Model {
        Model::from_definitions_with_interfaces(&self.types, &self.properties, &self.interfaces)
            .expect("inventory model projects")
            .with_governing_laws(&self.laws)
            .expect("inventory governing laws bind")
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
                .expect("inventory + core base artifact set is exact JSON")
                .canonical_json()
                .expect("inventory + core base artifact set canonicalizes"),
        )
        .expect("canonical inventory + core base set is UTF-8");
        InstalledDefinitionInput {
            canonical_set,
            registry: Vec::<String>::new(),
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
            |_: &Command<JsonObject>| panic!("inventory E2E actions are synchronous"),
        )
        .expect("SQLite inventory cell opens");
        cell.advance_to(LogicalTime::from_millis(KNOWN_AT));
        cell.install_definitions(model, self.install_input(), Some(0))
            .expect("admitted inventory definitions install");
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
        self.invoke_in_scope(
            cell,
            action_id,
            input,
            actor,
            test_scope_map(),
            idempotency_key,
        )
    }

    fn invoke_in_scope(
        &self,
        cell: &mut Cell,
        action_id: &str,
        input: &Value,
        actor: &str,
        scope: BTreeMap<String, String>,
        idempotency_key: &str,
    ) -> Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>> {
        let registered = &self.actions[action_id];
        let request = ActionCall {
            action: &registered.root,
            action_artifact_id: &registered.artifact_id,
            action_version: &registered.version,
            scope,
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
            trace: None,
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
            values: decode_json_object(values),
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

fn assert_law_violation<T: std::fmt::Debug>(
    result: Result<T, CommitError<norn_cell::CellError>>,
    expected_law_id: &str,
) {
    let error = result.expect_err("commit violating governing law must be refused");
    let CommitError::Law {
        reason: LawEvaluationError::Violated(failure),
        ..
    } = error
    else {
        panic!("expected CommitError::Law(Violated({expected_law_id})), got {error:?}");
    };
    let DataQualityRuleRef::Law { law_id, .. } = &failure.rule_ref else {
        panic!(
            "expected DataQualityRuleRef::Law({expected_law_id}), got {:?}",
            failure.rule_ref
        );
    };
    assert_eq!(
        law_id, expected_law_id,
        "expected law {expected_law_id}, got {law_id}"
    );
}

fn assert_proper_interval_violation<T: std::fmt::Debug>(
    result: Result<T, CommitError<norn_cell::CellError>>,
) {
    let error = result.expect_err("commit with improper TimeInterval must be refused");
    let CommitError::InvariantViolation { owner, id, .. } = error else {
        panic!(
            "expected CommitError::InvariantViolation(core.TimeInterval/proper_interval), got {error:?}"
        );
    };
    assert_eq!(
        owner,
        InvariantOwner::Interface(InterfaceName("core.TimeInterval".to_owned()))
    );
    assert_eq!(id.as_str(), "proper_interval");
}

fn assert_precondition_failed(
    result: Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>>,
    expected_fragment: &str,
    label: &str,
) {
    let Err(refusal) = result else {
        panic!("expected action.precondition_failed for {label}, got {result:?}");
    };
    let ActionRefusal::PreconditionFailed {
        precondition,
        reason,
    } = refusal.as_ref()
    else {
        panic!("expected ActionRefusal::PreconditionFailed for {label}, got {refusal:?}");
    };
    assert!(
        reason.is_none(),
        "expected boolean-false precondition (reason: None) for {label}, got reason={reason:?} on {precondition}"
    );
    assert!(
        precondition.contains(expected_fragment),
        "expected precondition for {label} to contain {expected_fragment:?}, got {precondition:?}"
    );
}

fn assert_immutable_state_refusal(
    fixture: &InventoryFixture,
    cell: &Cell,
    object_type: &str,
    id: &str,
    property: &str,
    value: RuntimeValue,
    expected_state: &str,
) {
    let key = FactKey::new(object_type, id);
    let state = cell
        .load(&key)
        .expect("object loads for immutability check");
    let slice = FactSlice::from([(key, state)]);
    let output = DecisionOutput {
        facts: vec![FactChange::Set {
            object_type: ObjectTypeName(object_type.to_owned()),
            key: id.to_owned(),
            property: PropertyId(property.to_owned()),
            value,
        }],
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn = DecisionTurn {
        id: format!("immutable-check:{object_type}:{id}:{expected_state}"),
        now: LogicalTime::from_millis(KNOWN_AT),
        world: World::Main,
        consumes: None,
    };
    let outcome =
        prepare_decision::<std::convert::Infallible>(&fixture.catalog(), &slice, &turn, &output);
    let Err(DecisionRefusal::ImmutableStateWrite {
        lifecycle,
        state: refused_state,
        ..
    }) = outcome
    else {
        panic!(
            "expected DecisionRefusal::ImmutableStateWrite(status={expected_state}) on {object_type}/{id}, got {outcome:?}"
        );
    };
    assert_eq!(lifecycle, "status");
    assert_eq!(refused_state, expected_state);
}

#[allow(
    clippy::too_many_arguments,
    reason = "concise fixture payload builder matching inventory.movement.record inputSchema"
)]
fn movement_payload(
    id: &str,
    action: &str,
    movement_kind: &str,
    quantity: &str,
    unit: &str,
    unit_cost: &str,
    provider_id: &str,
    receiver_id: &str,
    resource_id: &str,
) -> Value {
    json!({
        "id": id,
        "action": action,
        "movement_kind": movement_kind,
        "occurred_at": "2026-11-23T12:00:00Z",
        "quantity": quantity,
        "unit": unit,
        "unit_cost": unit_cost,
        "currency": "USD",
        "reference_code": format!("REF-{id}"),
        "providerId": provider_id,
        "receiverId": receiver_id,
        "resourceId": resource_id
    })
}

fn reservation_payload(
    id: &str,
    quantity: &str,
    unit: &str,
    resource_id: &str,
    holder_id: &str,
) -> Value {
    json!({
        "id": id,
        "quantity": quantity,
        "unit": unit,
        "currency": null,
        "expires_at": "2026-11-26T00:00:00Z",
        "resourceId": resource_id,
        "holderId": holder_id
    })
}

fn warehouse_fact(id: &str, code: &str, name: &str, status: &str) -> FactWrite {
    put_fact(
        "inventory.Warehouse",
        id,
        &json!({
            "id": id,
            "warehouse_code": code,
            "display_name": name,
            "kind": "organization",
            "status": status
        }),
    )
}

fn balance_fact(id: &str, resource_code: &str, quantity: i64, reserved: i64) -> FactWrite {
    put_fact(
        "inventory.StockBalance",
        id,
        &json!({
            "id": id,
            "resource_code": resource_code,
            "quantity": quantity,
            "reserved_quantity": reserved,
            "unit": "{each}",
            "currency": null,
            "valuation_method": "fifo"
        }),
    )
}

fn operational_facts() -> Vec<FactWrite> {
    vec![
        warehouse_fact("wh_central", "WH-CENTRAL-01", "Central Hub", "active"),
        warehouse_fact("wh_regional", "WH-REGIONAL-02", "Regional Annex", "active"),
        warehouse_fact("wh_frozen", "WH-FROZEN-03", "Frozen Depot", "frozen"),
        put_fact(
            "inventory.BinLocation",
            "bin_active",
            &json!({
                "id": "bin_active", "bin_code": "B-01-01", "zone": "MAIN",
                "max_capacity": 1000, "unit": "{each}", "status": "active"
            }),
        ),
        put_fact(
            "inventory.BinLocation",
            "bin_quarantined",
            &json!({
                "id": "bin_quarantined", "bin_code": "Q-09-01", "zone": "HOLD",
                "max_capacity": 200, "unit": "{each}", "status": "quarantined"
            }),
        ),
        put_fact(
            "inventory.Item",
            "item_valve",
            &json!({
                "id": "item_valve", "sku": "SKU-VALVE-09", "gtin": "00012345678905",
                "display_name": "Hydraulic Check Valve", "base_unit": "{each}",
                "valuation_method": "fifo", "track_lot": true, "reorder_point": 15,
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.LotBatch",
            "lot_2026_a",
            &json!({
                "id": "lot_2026_a", "lot_code": "LOT-2026-11-A",
                "manufactured_on": "2026-11-01", "expires_on": "2028-11-01",
                "status": "active"
            }),
        ),
        balance_fact("bal_central_valve", "SKU-VALVE-09@WH-CENTRAL-01", 120, 30),
        balance_fact("bal_quarantined_bin", "SKU-VALVE-09@Q-09-01", 40, 0),
        balance_fact("bal_frozen_owner", "SKU-VALVE-09@WH-FROZEN-03", 50, 0),
        balance_fact("bal_frozen_custodian", "SKU-VALVE-09@CUST-FROZEN", 50, 0),
        put_fact(
            "inventory.StockReservation",
            "res_release_target",
            &json!({
                "id": "res_release_target", "status": "open", "quantity": 5,
                "unit": "{each}", "currency": null, "expires_at": "2026-11-26T00:00:00Z"
            }),
        ),
        put_fact(
            "inventory.CycleCount",
            "cc_nov_2026",
            &json!({
                "id": "cc_nov_2026", "starts_at": "2026-11-23T08:00:00Z",
                "ends_at": "2026-11-23T11:30:00Z", "timezone": "UTC",
                "expected_quantity": 120, "counted_quantity": 118,
                "variance_quantity": -2, "status": "draft"
            }),
        ),
    ]
}

fn catalog_links() -> Vec<LinkWrite> {
    vec![
        add_link(
            "inventory.Warehouse",
            "wh_regional",
            "parent",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.BinLocation",
            "bin_active",
            "warehouse",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.BinLocation",
            "bin_quarantined",
            "warehouse",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.LotBatch",
            "lot_2026_a",
            "item",
            "inventory.Item",
            "item_valve",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_valve",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_valve",
            "item",
            "inventory.Item",
            "item_valve",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_valve",
            "bin",
            "inventory.BinLocation",
            "bin_active",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_valve",
            "lot",
            "inventory.LotBatch",
            "lot_2026_a",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_quarantined_bin",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_quarantined_bin",
            "item",
            "inventory.Item",
            "item_valve",
        ),
    ]
}

fn operational_record_links() -> Vec<LinkWrite> {
    vec![
        add_link(
            "inventory.StockBalance",
            "bal_quarantined_bin",
            "bin",
            "inventory.BinLocation",
            "bin_quarantined",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_frozen_owner",
            "owner",
            "inventory.Warehouse",
            "wh_frozen",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_frozen_owner",
            "item",
            "inventory.Item",
            "item_valve",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_frozen_custodian",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_frozen_custodian",
            "custodian",
            "inventory.Warehouse",
            "wh_frozen",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_frozen_custodian",
            "item",
            "inventory.Item",
            "item_valve",
        ),
        add_link(
            "inventory.StockReservation",
            "res_release_target",
            "resource",
            "inventory.StockBalance",
            "bal_central_valve",
        ),
        add_link(
            "inventory.StockReservation",
            "res_release_target",
            "holder",
            "inventory.Warehouse",
            "wh_regional",
        ),
        add_link(
            "inventory.CycleCount",
            "cc_nov_2026",
            "warehouse",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.CycleCount",
            "cc_nov_2026",
            "stock_balance",
            "inventory.StockBalance",
            "bal_central_valve",
        ),
    ]
}

fn seed_operational_world(cell: &mut Cell) {
    let mut seed = CellCommit::empty();
    seed.writes.extend(operational_facts());
    seed.links.extend(catalog_links());
    seed.links.extend(operational_record_links());
    cell.commit(seed)
        .expect("seed operational warehouses, bins, item, lot, stock balances, reservation, and cycle count");
}

#[test]
fn inventory_pack_admits_cleanly_against_core_base() {
    let base = decode_envelopes(&load_core_base_values());
    let artifacts = decode_envelopes(&load_inventory_pack_values());

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
        "inventory pack must admit cleanly against Core REA + TimeInterval base: {errors:#?}"
    );

    let law_ids: BTreeSet<&str> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    let expected_laws = [
        "inventory.Warehouse.kind_is_organization.law",
        "inventory.Item.non_negative_reorder.law",
        "inventory.LotBatch.valid_dates.law",
        "inventory.StockBalance.non_negative_and_reserved_bounded.law",
        "inventory.StockReservation.positive_quantity.law",
        "inventory.StockMovement.positive_quantity.law",
        "inventory.CycleCount.variance_conservation.law",
    ];
    for expected in expected_laws {
        assert!(
            law_ids.contains(expected),
            "missing governed law {expected} in admitted inventory pack: {law_ids:?}"
        );
    }

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
        "no checker findings against inventory governed laws: {law_findings:#?}"
    );
}

fn assert_catalog_entity_laws_refuse(cell: &mut Cell) {
    for (bal_id, quantity, reserved_quantity, unit) in [
        ("bal_negative", -1, 0, Value::String("{each}".to_owned())),
        (
            "bal_over_reserved",
            10,
            15,
            Value::String("{each}".to_owned()),
        ),
        ("bal_unitless", 10, 0, Value::Null),
    ] {
        let mut commit = CellCommit::empty();
        commit.writes.push(put_fact(
            "inventory.StockBalance",
            bal_id,
            &json!({
                "id": bal_id, "resource_code": "SKU-VALVE-09@WH-CENTRAL-01",
                "quantity": quantity, "reserved_quantity": reserved_quantity,
                "unit": unit, "currency": null, "valuation_method": "fifo"
            }),
        ));
        commit.links.extend([
            add_link(
                "inventory.StockBalance",
                bal_id,
                "owner",
                "inventory.Warehouse",
                "wh_central",
            ),
            add_link(
                "inventory.StockBalance",
                bal_id,
                "item",
                "inventory.Item",
                "item_valve",
            ),
        ]);
        assert_law_violation(
            cell.commit(commit),
            "inventory.StockBalance.non_negative_and_reserved_bounded",
        );
    }

    let mut person_wh = CellCommit::empty();
    person_wh.writes.push(put_fact(
        "inventory.Warehouse",
        "wh_person_invalid",
        &json!({
            "id": "wh_person_invalid", "warehouse_code": "WH-BAD-01",
            "display_name": "Invalid Person Warehouse", "kind": "person", "status": "active"
        }),
    ));
    assert_law_violation(
        cell.commit(person_wh),
        "inventory.Warehouse.kind_is_organization",
    );

    let mut bad_item = CellCommit::empty();
    bad_item.writes.push(put_fact(
        "inventory.Item",
        "item_bad_reorder",
        &json!({
            "id": "item_bad_reorder", "sku": "SKU-BAD-01", "gtin": null,
            "display_name": "Invalid Reorder Item", "base_unit": "{each}",
            "valuation_method": "moving_average", "track_lot": false,
            "reorder_point": -5, "status": "active"
        }),
    ));
    assert_law_violation(cell.commit(bad_item), "inventory.Item.non_negative_reorder");

    let mut bad_lot = CellCommit::empty();
    bad_lot.writes.push(put_fact(
        "inventory.LotBatch",
        "lot_reversed_dates",
        &json!({
            "id": "lot_reversed_dates", "lot_code": "LOT-BAD-DATE",
            "manufactured_on": "2028-11-01", "expires_on": "2026-11-01", "status": "active"
        }),
    ));
    bad_lot.links.push(add_link(
        "inventory.LotBatch",
        "lot_reversed_dates",
        "item",
        "inventory.Item",
        "item_valve",
    ));
    assert_law_violation(cell.commit(bad_lot), "inventory.LotBatch.valid_dates");
}

fn assert_cycle_count_laws_and_intervals_refuse(cell: &mut Cell) {
    let mut bad_variance = CellCommit::empty();
    bad_variance.writes.push(put_fact(
        "inventory.CycleCount",
        "cc_bad_variance",
        &json!({
            "id": "cc_bad_variance", "starts_at": "2026-11-23T08:00:00Z",
            "ends_at": "2026-11-23T10:00:00Z", "timezone": "UTC",
            "expected_quantity": 50, "counted_quantity": 47,
            "variance_quantity": 2, "status": "draft"
        }),
    ));
    bad_variance.links.extend([
        add_link(
            "inventory.CycleCount",
            "cc_bad_variance",
            "warehouse",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.CycleCount",
            "cc_bad_variance",
            "stock_balance",
            "inventory.StockBalance",
            "bal_central_valve",
        ),
    ]);
    assert_law_violation(
        cell.commit(bad_variance),
        "inventory.CycleCount.variance_conservation",
    );

    for (count_id, starts_at, ends_at) in [
        (
            "cc_equal_bounds",
            "2026-11-23T10:00:00Z",
            "2026-11-23T10:00:00Z",
        ),
        (
            "cc_reversed_bounds",
            "2026-11-23T12:00:00Z",
            "2026-11-23T10:00:00Z",
        ),
    ] {
        let mut bad_interval = CellCommit::empty();
        bad_interval.writes.push(put_fact(
            "inventory.CycleCount",
            count_id,
            &json!({
                "id": count_id, "starts_at": starts_at, "ends_at": ends_at,
                "timezone": "UTC", "expected_quantity": 50, "counted_quantity": 50,
                "variance_quantity": 0, "status": "draft"
            }),
        ));
        bad_interval.links.extend([
            add_link(
                "inventory.CycleCount",
                count_id,
                "warehouse",
                "inventory.Warehouse",
                "wh_central",
            ),
            add_link(
                "inventory.CycleCount",
                count_id,
                "stock_balance",
                "inventory.StockBalance",
                "bal_central_valve",
            ),
        ]);
        assert_proper_interval_violation(cell.commit(bad_interval));
    }
}

fn assert_reservation_and_movement_laws_refuse(cell: &mut Cell) {
    for (res_id, quantity, unit) in [
        ("res_zero", 0, Value::String("{each}".to_owned())),
        ("res_unitless", 5, Value::Null),
    ] {
        let mut bad_res = CellCommit::empty();
        bad_res.writes.push(put_fact(
            "inventory.StockReservation",
            res_id,
            &json!({
                "id": res_id, "status": "open", "quantity": quantity,
                "unit": unit, "currency": null, "expires_at": "2026-11-26T00:00:00Z"
            }),
        ));
        bad_res.links.extend([
            add_link(
                "inventory.StockReservation",
                res_id,
                "resource",
                "inventory.StockBalance",
                "bal_central_valve",
            ),
            add_link(
                "inventory.StockReservation",
                res_id,
                "holder",
                "inventory.Warehouse",
                "wh_central",
            ),
        ]);
        assert_law_violation(
            cell.commit(bad_res),
            "inventory.StockReservation.positive_quantity",
        );
    }

    for (mov_id, quantity, unit_cost, unit) in [
        ("mov_zero", 0, 10, Value::String("{each}".to_owned())),
        ("mov_neg_cost", 10, -1, Value::String("{each}".to_owned())),
        ("mov_unitless", 10, 10, Value::Null),
    ] {
        let mut bad_mov = CellCommit::empty();
        bad_mov.writes.push(put_fact(
            "inventory.StockMovement",
            mov_id,
            &json!({
                "id": mov_id, "action": "produce", "movement_kind": "receipt",
                "status": "recorded", "occurred_at": "2026-11-23T12:00:00Z",
                "quantity": quantity, "unit": unit, "unit_cost": unit_cost,
                "currency": "USD", "reference_code": "REF-BAD-001"
            }),
        ));
        bad_mov.links.extend([
            add_link(
                "inventory.StockMovement",
                mov_id,
                "provider",
                "inventory.Warehouse",
                "wh_central",
            ),
            add_link(
                "inventory.StockMovement",
                mov_id,
                "receiver",
                "inventory.Warehouse",
                "wh_central",
            ),
            add_link(
                "inventory.StockMovement",
                mov_id,
                "resource",
                "inventory.StockBalance",
                "bal_central_valve",
            ),
        ]);
        assert_law_violation(
            cell.commit(bad_mov),
            "inventory.StockMovement.positive_quantity",
        );
    }
}

#[test]
fn inventory_laws_and_time_interval_enforce_in_real_cell() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-laws.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);

    let snapshot_before = cell
        .store()
        .read()
        .dump()
        .expect("snapshot before refusals");
    assert_catalog_entity_laws_refuse(&mut cell);
    assert_cycle_count_laws_and_intervals_refuse(&mut cell);
    assert_reservation_and_movement_laws_refuse(&mut cell);

    assert_eq!(
        cell.store().read().dump().expect("snapshot after refusals"),
        snapshot_before
    );
}

fn movement_warehouse_and_pair_refusals() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "mov-refuse-frozen-wh",
            "slice.receiver.status == 'active'",
            movement_payload(
                "mov_frozen_wh",
                "transfer_custody",
                "transfer",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_frozen",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-frozen-owner",
            "slice.balance_owners",
            movement_payload(
                "mov_frozen_owner",
                "transfer_custody",
                "transfer",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_frozen_owner",
            ),
        ),
        (
            "mov-refuse-frozen-custodian",
            "slice.balance_custodians",
            movement_payload(
                "mov_frozen_cust",
                "transfer_custody",
                "transfer",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_frozen_custodian",
            ),
        ),
        (
            "mov-refuse-pair-consume-receipt",
            "input.movement_kind == 'receipt'",
            movement_payload(
                "mov_bad_pair_1",
                "consume",
                "receipt",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-pair-produce-issue",
            "input.movement_kind == 'issue'",
            movement_payload(
                "mov_bad_pair_2",
                "produce",
                "issue",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-pair-transfer-adjustment",
            "input.movement_kind == 'adjustment'",
            movement_payload(
                "mov_bad_pair_3",
                "transfer_custody",
                "adjustment",
                "10",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
    ]
}

fn movement_qty_and_unit_refusals() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "mov-refuse-zero-qty",
            "input.quantity > 0",
            movement_payload(
                "mov_zero_qty",
                "produce",
                "receipt",
                "0",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-neg-qty",
            "input.quantity > 0",
            movement_payload(
                "mov_neg_qty",
                "produce",
                "receipt",
                "-5",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-neg-cost",
            "input.unit_cost >= 0",
            movement_payload(
                "mov_neg_cost",
                "produce",
                "receipt",
                "10",
                "{each}",
                "-1.00",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-unit-mismatch",
            "slice.resource.unit",
            movement_payload(
                "mov_unit_mismatch",
                "transfer_custody",
                "transfer",
                "10",
                "kg",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
    ]
}

fn movement_availability_and_bin_refusals() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "mov-refuse-over-issue",
            "slice.resource.reserved_quantity",
            movement_payload(
                "mov_over_issue",
                "consume",
                "issue",
                "95",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-over-consume-adjustment",
            "slice.resource.reserved_quantity",
            movement_payload(
                "mov_over_adj",
                "consume",
                "adjustment",
                "95",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_central_valve",
            ),
        ),
        (
            "mov-refuse-quarantined-bin",
            "slice.bins",
            movement_payload(
                "mov_qbin",
                "consume",
                "issue",
                "5",
                "{each}",
                "14.50",
                "wh_central",
                "wh_regional",
                "bal_quarantined_bin",
            ),
        ),
    ]
}

#[test]
fn inventory_movement_preconditions_enforce() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-movement-guards.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);

    let cases = movement_warehouse_and_pair_refusals()
        .into_iter()
        .chain(movement_qty_and_unit_refusals())
        .chain(movement_availability_and_bin_refusals());
    for (key, expected_fragment, payload) in cases {
        let result = fixture.invoke(
            &mut cell,
            "inventory.movement.record",
            &payload,
            "principal:warehouse_clerk",
            key,
        );
        assert_precondition_failed(result, expected_fragment, key);
    }
}

fn assert_transfer_links(cell: &Cell, movement_id: &str) {
    for (link, target_type, target_id) in [
        ("resource", "inventory.StockBalance", "bal_central_valve"),
        ("receiver", "inventory.Warehouse", "wh_regional"),
    ] {
        let link_key = LinkKey::new(
            FactKey::new("inventory.StockMovement", movement_id),
            link,
            FactKey::new(target_type, target_id),
        );
        assert!(
            cell.load_link(&link_key)
                .expect("movement link loads")
                .exists()
        );
    }
}

#[test]
fn inventory_movement_create_only_and_recorded_immutability_enforce() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-movement-immut.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);

    // Inflow adjustment (`produce`) is not bounded by current unreserved stock.
    let inflow_adj = movement_payload(
        "mov_adj_inflow_001",
        "produce",
        "adjustment",
        "95",
        "{each}",
        "14.50",
        "wh_central",
        "wh_central",
        "bal_central_valve",
    );
    fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &inflow_adj,
            "principal:warehouse_clerk",
            "mov-adj-inflow-1",
        )
        .expect("produce adjustment succeeds");

    let transfer = movement_payload(
        "mov_transfer_001",
        "transfer_custody",
        "transfer",
        "25",
        "{each}",
        "14.50",
        "wh_central",
        "wh_regional",
        "bal_central_valve",
    );
    fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &transfer,
            "principal:warehouse_clerk",
            "mov-record-1",
        )
        .expect("warehouse clerk records stock transfer");
    assert_eq!(
        status_of(&cell, "inventory.StockMovement", "mov_transfer_001"),
        "recorded"
    );
    assert_transfer_links(&cell, "mov_transfer_001");

    let replayed = fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &transfer,
            "principal:warehouse_clerk",
            "mov-record-1",
        )
        .expect("exact retry replays");
    assert!(matches!(replayed, ActionOutcome::Replayed { .. }));
    let dup = fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &transfer,
            "principal:warehouse_clerk",
            "mov-record-dup",
        )
        .expect_err("duplicate StockMovement ID must be refused by create_only");
    assert_eq!(dup.code(), "action.write_mode_failed");
    assert_immutable_state_refusal(
        &fixture,
        &cell,
        "inventory.StockMovement",
        "mov_transfer_001",
        "reference_code",
        RuntimeValue::from("REF-TAMPERED"),
        "recorded",
    );
}

fn reservation_refusal_cases() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "res-refuse-frozen-wh",
            "slice.warehouse.status == 'active'",
            reservation_payload(
                "res_frozen_wh",
                "10",
                "{each}",
                "bal_central_valve",
                "wh_frozen",
            ),
        ),
        (
            "res-refuse-frozen-owner",
            "slice.balance_owners",
            reservation_payload(
                "res_frozen_owner",
                "10",
                "{each}",
                "bal_frozen_owner",
                "wh_regional",
            ),
        ),
        (
            "res-refuse-frozen-custodian",
            "slice.balance_custodians",
            reservation_payload(
                "res_frozen_cust",
                "10",
                "{each}",
                "bal_frozen_custodian",
                "wh_regional",
            ),
        ),
        (
            "res-refuse-unit-mismatch",
            "slice.resource.unit",
            reservation_payload(
                "res_bad_unit",
                "10",
                "kg",
                "bal_central_valve",
                "wh_regional",
            ),
        ),
        (
            "res-refuse-over-available",
            "slice.resource.reserved_quantity",
            reservation_payload(
                "res_over",
                "95",
                "{each}",
                "bal_central_valve",
                "wh_regional",
            ),
        ),
        (
            "res-refuse-quarantined-bin",
            "slice.bins",
            reservation_payload(
                "res_qbin",
                "5",
                "{each}",
                "bal_quarantined_bin",
                "wh_regional",
            ),
        ),
    ]
}

#[test]
fn inventory_reservation_guards_and_lifecycle_enforce() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-reservations.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);

    for (key, expected_fragment, payload) in reservation_refusal_cases() {
        let result = fixture.invoke(
            &mut cell,
            "inventory.reservation.create",
            &payload,
            "principal:warehouse_clerk",
            key,
        );
        assert_precondition_failed(result, expected_fragment, key);
    }

    let valid_res = reservation_payload(
        "res_created_001",
        "15",
        "{each}",
        "bal_central_valve",
        "wh_regional",
    );
    fixture
        .invoke(
            &mut cell,
            "inventory.reservation.create",
            &valid_res,
            "principal:warehouse_clerk",
            "res-create-1",
        )
        .expect("warehouse clerk creates valid open reservation");
    assert_eq!(
        status_of(&cell, "inventory.StockReservation", "res_created_001"),
        "open"
    );
    fixture
        .invoke(
            &mut cell,
            "inventory.reservation.consume",
            &json!({"reservationId": "res_created_001"}),
            "principal:warehouse_clerk",
            "res-consume-1",
        )
        .expect("warehouse clerk consumes open reservation");
    assert_eq!(
        status_of(&cell, "inventory.StockReservation", "res_created_001"),
        "consumed"
    );
    fixture
        .invoke(
            &mut cell,
            "inventory.reservation.release",
            &json!({"reservationId": "res_release_target"}),
            "principal:warehouse_clerk",
            "res-release-1",
        )
        .expect("warehouse clerk releases open reservation");
    assert_eq!(
        status_of(&cell, "inventory.StockReservation", "res_release_target"),
        "released"
    );
}

#[test]
fn inventory_lot_quarantine_and_recall_enforce() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-lots.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);

    fixture
        .invoke(
            &mut cell,
            "inventory.lot.quarantine",
            &json!({"lotId": "lot_2026_a"}),
            "principal:warehouse_clerk",
            "lot-quarantine-1",
        )
        .expect("warehouse clerk quarantines active lot");
    assert_eq!(
        status_of(&cell, "inventory.LotBatch", "lot_2026_a"),
        "quarantined"
    );

    let qlot_mov = movement_payload(
        "mov_qlot",
        "transfer_custody",
        "transfer",
        "5",
        "{each}",
        "14.50",
        "wh_central",
        "wh_regional",
        "bal_central_valve",
    );
    assert_precondition_failed(
        fixture.invoke(
            &mut cell,
            "inventory.movement.record",
            &qlot_mov,
            "principal:warehouse_clerk",
            "mov-refuse-qlot",
        ),
        "slice.lots",
        "mov-refuse-qlot",
    );
    let qlot_res = reservation_payload(
        "res_qlot",
        "5",
        "{each}",
        "bal_central_valve",
        "wh_regional",
    );
    assert_precondition_failed(
        fixture.invoke(
            &mut cell,
            "inventory.reservation.create",
            &qlot_res,
            "principal:warehouse_clerk",
            "res-refuse-qlot",
        ),
        "slice.lots",
        "res-refuse-qlot",
    );

    let clerk_recall = fixture
        .invoke(
            &mut cell,
            "inventory.lot.recall",
            &json!({"lotId": "lot_2026_a"}),
            "principal:warehouse_clerk",
            "lot-recall-clerk-denied",
        )
        .expect_err("warehouse clerk denied inventory.lot.recall");
    assert_eq!(clerk_recall.code(), "action.permission_denied");
    fixture
        .invoke(
            &mut cell,
            "inventory.lot.recall",
            &json!({"lotId": "lot_2026_a"}),
            "principal:quality_inspector",
            "lot-recall-inspector-1",
        )
        .expect("quality inspector recalls quarantined lot");
    assert_eq!(
        status_of(&cell, "inventory.LotBatch", "lot_2026_a"),
        "recalled"
    );
    assert_immutable_state_refusal(
        &fixture,
        &cell,
        "inventory.LotBatch",
        "lot_2026_a",
        "lot_code",
        RuntimeValue::from("LOT-2026-11-A-TAMPERED"),
        "recalled",
    );
}

fn assert_cross_scope_denials(fixture: &InventoryFixture, cell: &mut Cell) {
    let wrong_org = BTreeMap::from([
        ("orgId".to_owned(), "org_other_tenant".to_owned()),
        ("environmentId".to_owned(), ENV_ID.to_owned()),
    ]);
    let wrong_org_post = fixture
        .invoke_in_scope(
            cell,
            "inventory.cycle_count.post",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:inventory_manager",
            wrong_org.clone(),
            "cc-post-wrong-org",
        )
        .expect_err("cross-org post denied");
    assert_eq!(wrong_org_post.code(), "action.permission_denied");

    let wrong_env = BTreeMap::from([
        ("orgId".to_owned(), ORG_ID.to_owned()),
        ("environmentId".to_owned(), "prod".to_owned()),
    ]);
    let wrong_env_post = fixture
        .invoke_in_scope(
            cell,
            "inventory.cycle_count.post",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:inventory_manager",
            wrong_env,
            "cc-post-wrong-env",
        )
        .expect_err("cross-env post denied");
    assert_eq!(wrong_env_post.code(), "action.permission_denied");

    let missing_env = BTreeMap::from([("orgId".to_owned(), ORG_ID.to_owned())]);
    let missing_env_post = fixture
        .invoke_in_scope(
            cell,
            "inventory.cycle_count.post",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:inventory_manager",
            missing_env,
            "cc-post-missing-env",
        )
        .expect_err("missing environmentId refused");
    assert_eq!(missing_env_post.code(), "action.missing_scope");

    let xorg_mov = movement_payload(
        "mov_cross_org",
        "produce",
        "receipt",
        "5",
        "{each}",
        "14.50",
        "wh_central",
        "wh_regional",
        "bal_central_valve",
    );
    let xorg_result = fixture
        .invoke_in_scope(
            cell,
            "inventory.movement.record",
            &xorg_mov,
            "principal:warehouse_clerk",
            wrong_org,
            "mov-cross-org-denied",
        )
        .expect_err("cross-org slice read fails closed");
    assert_eq!(xorg_result.code(), "action.target_not_found");
}

fn assert_durable_reopen(fixture: &InventoryFixture, path: &Path, cell: Cell) {
    let dump = cell.store().read().dump().expect("final dump");
    let head = cell
        .store()
        .read()
        .verify_ledger()
        .expect("ledger verifies");
    drop(cell);
    let reopened = Cell::open(
        path,
        fixture.model(),
        StoreOptions::default(),
        SCOPE_KEY,
        |_: &Command<JsonObject>| panic!("reopen executes no commands"),
    )
    .expect("reopened inventory cell");
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

fn assert_cycle_count_submit_and_approve(fixture: &InventoryFixture, cell: &mut Cell) {
    let cc_input = json!({"cycleCountId": "cc_nov_2026"});
    fixture
        .invoke(
            cell,
            "inventory.cycle_count.submit",
            &cc_input,
            "principal:warehouse_clerk",
            "cc-submit-1",
        )
        .expect("warehouse clerk submits draft cycle count");
    assert_eq!(
        status_of(cell, "inventory.CycleCount", "cc_nov_2026"),
        "submitted"
    );

    for (actor, key) in [
        ("principal:warehouse_clerk", "cc-approve-clerk-denied"),
        ("principal:quality_inspector", "cc-approve-inspector-denied"),
    ] {
        let denied = fixture
            .invoke(cell, "inventory.cycle_count.approve", &cc_input, actor, key)
            .expect_err("non-manager denied inventory.cycle_count.approve");
        assert_eq!(denied.code(), "action.permission_denied");
    }

    fixture
        .invoke(
            cell,
            "inventory.cycle_count.approve",
            &cc_input,
            "principal:inventory_manager",
            "cc-approve-manager-1",
        )
        .expect("inventory manager approves submitted cycle count");
    assert_eq!(
        status_of(cell, "inventory.CycleCount", "cc_nov_2026"),
        "approved"
    );
    assert_immutable_state_refusal(
        fixture,
        cell,
        "inventory.CycleCount",
        "cc_nov_2026",
        "timezone",
        RuntimeValue::from("Europe/London"),
        "approved",
    );
}

#[test]
fn inventory_cycle_count_immutability_and_scope_cedar_enforce() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-cycle-count.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_operational_world(&mut cell);
    assert_cycle_count_submit_and_approve(&fixture, &mut cell);

    let cc_input = json!({"cycleCountId": "cc_nov_2026"});
    let clerk_post = fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.post",
            &cc_input,
            "principal:warehouse_clerk",
            "cc-post-clerk-denied",
        )
        .expect_err("warehouse clerk denied inventory.cycle_count.post");
    assert_eq!(clerk_post.code(), "action.permission_denied");
    assert_cross_scope_denials(&fixture, &mut cell);

    fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.post",
            &cc_input,
            "principal:inventory_manager",
            "cc-post-manager-1",
        )
        .expect("inventory manager posts approved cycle count");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "posted"
    );
    assert_immutable_state_refusal(
        &fixture,
        &cell,
        "inventory.CycleCount",
        "cc_nov_2026",
        "timezone",
        RuntimeValue::from("America/New_York"),
        "posted",
    );
    assert_durable_reopen(&fixture, &path, cell);
}
