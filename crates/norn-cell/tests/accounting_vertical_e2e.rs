#![cfg(feature = "native")]
//! End-to-end `norn-cell` execution suite for the jurisdiction-agnostic
//! Accounting Domain Pack (`registry/packs/accounting/`): static admission
//! against the Core REA + `core.TimeInterval` base, real SQLite cell definition
//! installation with checker receipt, double-entry journal & fiscal period
//! governance (including `LAW-LINK-MANY-1` line-sum reconciliation), invoice
//! conservation and no-void-with-payments enforcement, `Payment` create-only
//! duplicate-ID refusal and single-state immutable lifecycle refusal, and
//! fail-closed Cedar `AccessIR` with separation of duties on journal posting.
//!
//! Boundary notes: `lifecycle.immutable` is enforced during kernel decision
//! preparation (`FactChange::Set`), not raw `CellHost::commit` writes or
//! `FactChange::Delete`. `accounting.payment.record` is a single-object
//! `create_only` `upsert_input_object` write capturing payment evidence; it
//! does not mutate `Invoice.amount_paid`/`status` or `MonetaryAccount.quantity`
//! in the same commit.

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
    PropertyId, PropertyTypeDef, ResourceLimitProfile, RuntimeValue, lifecycle_codes,
};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::governing_laws::LawEvaluationError;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, CommitError, CommitReceipt, DecisionRefusal, DecisionTurn, FactEvent, FactKey,
    FactMutation, FactSlice, FactVersion, FactWrite, LinkKey, LinkMutation, LinkWrite, LogicalTime,
    TypeCatalog, World, commit_action, prepare_decision,
};
use norn_projections::CedarAttribute;
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE_KEY: &str = "org_acme_ledger/dev";
const ORG_ID: &str = "org_acme_ledger";
const ENV_ID: &str = "dev";
const KNOWN_AT: u64 = 1_795_420_800_000;
const PRINCIPAL_ID_ATTRIBUTE: &str = "id";

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

#[allow(clippy::panic, reason = "accounting test fixture file reader")]
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

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "accounting test fixture artifact directory loader"
)]
fn load_accounting_pack_values() -> Vec<Value> {
    let dir = repo_root().join("registry/packs/accounting/artifacts");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("artifact dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|path| read_json_value(path)).collect()
}

#[allow(
    clippy::expect_used,
    reason = "accounting test fixture envelope decoder"
)]
fn decode_envelopes(values: &[Value]) -> Vec<ArtifactEnvelope> {
    values
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("artifact envelope decodes"))
        .collect()
}

#[allow(
    clippy::expect_used,
    reason = "accounting test fixture norn.ActionCall schema builder"
)]
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

struct DecodedDefinitions {
    types: Vec<ObjectTypeDef>,
    properties: Vec<PropertyTypeDef>,
    interfaces: Vec<InterfaceDef>,
    actions: BTreeMap<String, RegisteredAction>,
    policies: Vec<AccessIr>,
}

#[allow(
    clippy::expect_used,
    reason = "accounting test fixture definition decoder"
)]
fn decode_pack_definitions(base_values: &[Value], artifact_values: &[Value]) -> DecodedDefinitions {
    let mut types = Vec::new();
    let mut properties = Vec::new();
    let mut interfaces = Vec::new();
    let mut actions = BTreeMap::new();
    let mut policies = Vec::new();

    for document in base_values.iter().chain(artifact_values.iter()) {
        let payload = document.get("payload").expect("artifact payload");
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
        if document.get("kind").and_then(Value::as_str) == Some("action") {
            let root: ActionRoot =
                serde_json::from_value(payload.clone()).expect("action root decodes");
            let artifact_id = document
                .get("artifactId")
                .and_then(Value::as_str)
                .expect("action artifactId")
                .to_owned();
            let version = document
                .get("version")
                .and_then(Value::as_str)
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
        if document.get("kind").and_then(Value::as_str) == Some("policy") {
            let policy: AccessIr =
                serde_json::from_value(payload.clone()).expect("policy root decodes");
            policies.push(policy);
        }
    }
    types.push(action_call_type());

    DecodedDefinitions {
        types,
        properties,
        interfaces,
        actions,
        policies,
    }
}

struct AccountingPrincipals {
    pdp: Pdp,
    resource_attributes: BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_id: bool,
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
        "principal:accountant" => vec!["accountant".to_owned()],
        "principal:senior_accountant" => vec!["senior_accountant".to_owned()],
        "principal:controller" => vec!["controller".to_owned()],
        "principal:owner" => vec!["owner".to_owned()],
        _ => Vec::<String>::new(),
    }
}

impl AccountingPrincipals {
    #[allow(clippy::expect_used, reason = "accounting test fixture PDP setup")]
    fn new(
        types: &[ObjectTypeDef],
        interfaces: &[InterfaceDef],
        properties: &[PropertyTypeDef],
        actions: &BTreeMap<String, RegisteredAction>,
        policies: &[AccessIr],
    ) -> Self {
        let action_roots: Vec<ActionRoot> = actions.values().map(|a| a.root.clone()).collect();
        let plain = norn_projections::project_cedar_schema_with_catalog(
            types,
            interfaces,
            properties,
            &action_roots,
        );
        let principal_attrs = authz::infer_principal_attributes(policies, &plain)
            .expect("accounting principal attributes infer");
        let projection =
            norn_projections::project_cedar_schema_with_catalog_and_principal_attributes(
                types,
                interfaces,
                properties,
                &action_roots,
                &principal_attrs,
            );
        let host_schema = norn_projections::project_host_access_schema(types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
                .expect("accounting Cedar schema loads");
        let compiled = authz::compile_all_projected(&schema, policies, &projection)
            .expect("accounting AccessIR policies compile against projected Cedar schema");

        // Grant every test principal broad action capabilities so the compiled
        // Cedar AccessIR rules (roles + separation of duties) decide allow vs deny.
        let mut all_capabilities: Vec<String> =
            action_roots.iter().map(|a| a.action_id.clone()).collect();
        all_capabilities.push("norn.read".to_owned());

        let grants: Vec<CapabilityGrant> = [
            "principal:viewer",
            "principal:accountant",
            "principal:senior_accountant",
            "principal:controller",
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

        let resource_attributes = projection
            .attributes
            .into_iter()
            .map(|(object_type, attributes)| {
                let scalar = attributes
                    .into_iter()
                    .filter(|(_, kind)| {
                        matches!(
                            kind,
                            CedarAttribute::Bool | CedarAttribute::Long | CedarAttribute::String
                        )
                    })
                    .collect();
                (object_type, scalar)
            })
            .collect();

        Self {
            pdp: Pdp::new(compiled, &grants).expect("accounting PDP constructs"),
            resource_attributes,
            principal_id: principal_attrs.contains_key(PRINCIPAL_ID_ATTRIBUTE),
        }
    }

    fn resource_values(
        &self,
        object_type: &str,
        values: Option<&JsonObject>,
    ) -> BTreeMap<String, AttrValue> {
        let (Some(projected), Some(values)) = (self.resource_attributes.get(object_type), values)
        else {
            return BTreeMap::new();
        };
        projected
            .iter()
            .filter_map(|(name, kind)| {
                let value = values.get(name)?;
                let attribute = match (kind, value) {
                    (CedarAttribute::String, RuntimeValue::String(text)) => {
                        AttrValue::String(text.clone())
                    }
                    (CedarAttribute::Bool, RuntimeValue::Bool(flag)) => AttrValue::Bool(*flag),
                    (CedarAttribute::Long, RuntimeValue::Number(number)) => {
                        AttrValue::Long(number.to_string().parse().ok()?)
                    }
                    _ => return None,
                };
                Some((name.clone(), attribute))
            })
            .collect()
    }

    fn access_request(
        &self,
        actor: &str,
        action: &str,
        target: &FactKey,
        target_values: Option<&JsonObject>,
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
        let principal_attributes = if self.principal_id {
            BTreeMap::from([(
                PRINCIPAL_ID_ATTRIBUTE.to_owned(),
                AttrValue::String(actor.to_owned()),
            )])
        } else {
            BTreeMap::new()
        };
        let resource_attributes = self.resource_values(target.object_type.as_str(), target_values);
        Ok(AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.to_owned(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: actor_roles(actor),
                attributes: principal_attributes,
            },
            scope: Some(request_scope.clone()),
            capability: action.to_owned(),
            action_id: action.to_owned(),
            resource: ResourceInput {
                object_type: target.object_type.as_str().to_owned(),
                id: target.key.clone(),
                scope: request_scope,
                attributes: resource_attributes,
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

impl ActionAuthorizer for AccountingPrincipals {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        PdpActionAuthorizer::new(self.pdp.clone(), |req: &ActionAuthzRequest<'_>| {
            self.access_request(
                req.actor,
                &req.action.action_id,
                req.target,
                req.target_values,
                req.scope,
            )
        })
        .authorize(request)
    }

    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
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
        if request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
            || request.scope.get("environmentId").map(String::as_str) != Some(ENV_ID)
        {
            return Err(ActionDenial::Invisible);
        }
        Ok(())
    }
}

struct AccountingFixture {
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
    principals: AccountingPrincipals,
}

impl AccountingFixture {
    #[allow(
        clippy::expect_used,
        reason = "accounting test fixture admission and receipt loader"
    )]
    fn load() -> Self {
        let base_values = load_core_base_values();
        let artifact_values = load_accounting_pack_values();
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
        .expect("accounting pack admission receipt");
        assert!(
            checked.receipt.admitted,
            "accounting pack must admit with zero error diagnostics: {:#?}",
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

        let decoded = decode_pack_definitions(&base_values, &artifact_values);
        let laws: Vec<ArtifactEnvelope> = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Law)
            .cloned()
            .collect();
        let principals = AccountingPrincipals::new(
            &decoded.types,
            &decoded.interfaces,
            &decoded.properties,
            &decoded.actions,
            &decoded.policies,
        );

        Self {
            base_values,
            artifact_values,
            artifacts,
            admission_receipt,
            receipt_sha256,
            types: decoded.types,
            properties: decoded.properties,
            interfaces: decoded.interfaces,
            actions: decoded.actions,
            laws,
            principals,
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "accounting test fixture model projection"
    )]
    fn model(&self) -> Model {
        Model::from_definitions_with_interfaces(&self.types, &self.properties, &self.interfaces)
            .expect("accounting model projects")
            .with_governing_laws(&self.laws)
            .expect("accounting governing laws bind")
    }

    fn catalog(&self) -> TypeCatalog<'_> {
        TypeCatalog::new(&self.types)
            .with_property_types(&self.properties)
            .with_interfaces(&self.interfaces)
            .with_governing_laws(&self.laws)
    }

    #[allow(
        clippy::expect_used,
        reason = "accounting test fixture canonical definition bundle"
    )]
    fn install_input(&self) -> InstalledDefinitionInput {
        let mut combined = self.artifact_values.clone();
        combined.extend(self.base_values.iter().cloned());
        let canonical_set = String::from_utf8(
            RuntimeValue::from_json(Value::Array(combined))
                .expect("accounting + core base artifact set is exact JSON")
                .canonical_json()
                .expect("accounting + core base artifact set canonicalizes"),
        )
        .expect("canonical accounting + core base set is UTF-8");
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

    #[allow(
        clippy::expect_used,
        clippy::panic,
        reason = "accounting test fixture SQLite cell opener"
    )]
    fn open_installed_cell(&self, path: &Path) -> Cell {
        let model = self.model();
        let mut cell = Cell::open(
            path,
            model.clone(),
            StoreOptions::default(),
            SCOPE_KEY,
            |_: &Command<JsonObject>| panic!("accounting E2E actions are synchronous"),
        )
        .expect("SQLite accounting cell opens");
        cell.advance_to(LogicalTime::from_millis(KNOWN_AT));
        cell.install_definitions(model, self.install_input(), Some(0))
            .expect("admitted accounting definitions install");
        cell
    }

    #[allow(clippy::expect_used, reason = "accounting test action invoker")]
    fn invoke(
        &self,
        cell: &mut Cell,
        action_id: &str,
        input: &Value,
        actor: &str,
        idempotency_key: &str,
    ) -> Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>> {
        let registered = self
            .actions
            .get(action_id)
            .expect("registered accounting action");
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
            trace: None,
        };
        commit_action(cell, &self.catalog(), &request, &self.principals).map_err(Box::new)
    }
}

#[allow(clippy::expect_used, reason = "accounting test fixture JSON helper")]
fn decode_json_object(value: &Value) -> JsonObject {
    serde_json::from_value(value.clone()).expect("JSON object decodes")
}

#[allow(clippy::expect_used, reason = "accounting test fixture fact helper")]
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

#[allow(clippy::expect_used, reason = "accounting test fixture status reader")]
fn status_of(cell: &Cell, object_type: &str, id: &str) -> String {
    cell.load(&FactKey::new(object_type, id))
        .expect("object loads")
        .values
        .expect("object exists")
        .get("status")
        .expect("status property")
        .as_str()
        .expect("status is text")
        .to_owned()
}

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "accounting test assertion helper"
)]
fn assert_law_refusal(
    result: Result<CommitReceipt, CommitError<norn_cell::CellError>>,
    expected_law_id: &str,
) {
    let err = result.expect_err("expected governing law refusal");
    assert_eq!(
        err.code(),
        "core.laws.violated",
        "unexpected commit error for {expected_law_id}: {err}"
    );
    match err {
        CommitError::Law {
            reason: LawEvaluationError::Violated(failure),
            ..
        } => match failure.rule_ref {
            DataQualityRuleRef::Law { law_id, .. } => {
                assert_eq!(law_id, expected_law_id);
            }
            other => panic!("expected Law rule_ref for {expected_law_id}, got {other:?}"),
        },
        other => panic!("expected CommitError::Law for {expected_law_id}, got {other:?}"),
    }
}

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "accounting test assertion helper"
)]
fn assert_action_law_refusal(
    result: Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>>,
    expected_law_id: &str,
) {
    let err = *result.expect_err("expected action governing law refusal");
    assert_eq!(
        err.code(),
        "core.laws.violated",
        "unexpected action refusal for {expected_law_id}: {err}"
    );
    match err {
        ActionRefusal::Decision(DecisionRefusal::Law {
            reason: LawEvaluationError::Violated(failure),
            ..
        }) => match failure.rule_ref {
            DataQualityRuleRef::Law { law_id, .. } => {
                assert_eq!(law_id, expected_law_id);
            }
            other => panic!("expected Law rule_ref for {expected_law_id}, got {other:?}"),
        },
        other => panic!(
            "expected ActionRefusal::Decision(DecisionRefusal::Law) for {expected_law_id}, got {other:?}"
        ),
    }
}

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "accounting test assertion helper"
)]
fn assert_time_interval_refusal(result: Result<CommitReceipt, CommitError<norn_cell::CellError>>) {
    let err = result.expect_err("expected core.TimeInterval invariant refusal");
    assert_eq!(
        err.code(),
        "commit.invariant_violation",
        "unexpected commit error for core.TimeInterval: {err}"
    );
    match err {
        CommitError::InvariantViolation { owner, id, .. } => {
            assert_eq!(
                owner,
                InvariantOwner::Interface(InterfaceName("core.TimeInterval".to_owned()))
            );
            assert_eq!(id.as_str(), "proper_interval");
        }
        other => panic!("expected CommitError::InvariantViolation, got {other:?}"),
    }
}

fn base_book_and_counterparty_facts() -> Vec<FactWrite> {
    vec![
        put_fact(
            "accounting.Book",
            "book_main",
            &json!({
                "id": "book_main",
                "code": "IFRS-MAIN",
                "display_name": "Primary IFRS Ledger",
                "functional_currency": "USD",
                "accounting_standard": "ifrs",
                "status": "active"
            }),
        ),
        put_fact(
            "accounting.Counterparty",
            "cp_company",
            &json!({
                "id": "cp_company",
                "display_name": "Acme Operating Entity",
                "kind": "organization",
                "counterparty_code": "ENT-001",
                "counterparty_type": "entity"
            }),
        ),
        put_fact(
            "accounting.Counterparty",
            "cp_customer",
            &json!({
                "id": "cp_customer",
                "display_name": "Northwind Trading Co.",
                "kind": "organization",
                "counterparty_code": "CUST-100",
                "counterparty_type": "customer"
            }),
        ),
        put_fact(
            "accounting.Counterparty",
            "cp_bank",
            &json!({
                "id": "cp_bank",
                "display_name": "First Clearing Bank",
                "kind": "organization",
                "counterparty_code": "BANK-01",
                "counterparty_type": "bank"
            }),
        ),
    ]
}

fn base_account_facts() -> Vec<FactWrite> {
    vec![
        put_fact(
            "accounting.Account",
            "acct_assets_group",
            &json!({
                "id": "acct_assets_group",
                "account_code": "1000",
                "display_name": "Assets",
                "account_class": "asset",
                "normal_balance": "debit",
                "currency": "USD",
                "is_group": true,
                "allow_direct_posting": false,
                "status": "active"
            }),
        ),
        put_fact(
            "accounting.Account",
            "acct_cash",
            &json!({
                "id": "acct_cash",
                "account_code": "1010",
                "display_name": "Operating Cash",
                "account_class": "asset",
                "normal_balance": "debit",
                "currency": "USD",
                "is_group": false,
                "allow_direct_posting": true,
                "status": "active"
            }),
        ),
        put_fact(
            "accounting.Account",
            "acct_revenue",
            &json!({
                "id": "acct_revenue",
                "account_code": "4010",
                "display_name": "Product Revenue",
                "account_class": "revenue",
                "normal_balance": "credit",
                "currency": "USD",
                "is_group": false,
                "allow_direct_posting": true,
                "status": "active"
            }),
        ),
    ]
}

fn base_period_and_resource_facts() -> Vec<FactWrite> {
    vec![
        put_fact(
            "accounting.FiscalPeriod",
            "fp_2026_11",
            &json!({
                "id": "fp_2026_11",
                "period_code": "2026-11",
                "starts_at": "2026-11-01T00:00:00Z",
                "ends_at": "2026-12-01T00:00:00Z",
                "timezone": "UTC",
                "status": "open"
            }),
        ),
        put_fact(
            "accounting.FiscalPeriod",
            "fp_2026_12",
            &json!({
                "id": "fp_2026_12",
                "period_code": "2026-12",
                "starts_at": "2026-12-01T00:00:00Z",
                "ends_at": "2027-01-01T00:00:00Z",
                "timezone": "UTC",
                "status": "open"
            }),
        ),
        put_fact(
            "accounting.MonetaryAccount",
            "ma_operating_usd",
            &json!({
                "id": "ma_operating_usd",
                "resource_code": "CASH-USD-01",
                "display_name": "USD Operating Cash Account",
                "quantity": 10_000,
                "unit": null,
                "currency": "USD",
                "valuation_method": null,
                "account_kind": "bank"
            }),
        ),
        put_fact(
            "accounting.FinancialEvent",
            "fe_sale_001",
            &json!({
                "id": "fe_sale_001",
                "action": "transfer_ownership",
                "occurred_at": "2026-11-15T12:00:00Z",
                "quantity": 1000,
                "unit": null,
                "currency": "USD",
                "reference_no": "SALE-2026-001"
            }),
        ),
    ]
}

fn base_ledger_links() -> Vec<LinkWrite> {
    vec![
        add_link(
            "accounting.Account",
            "acct_assets_group",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.Account",
            "acct_cash",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.Account",
            "acct_cash",
            "parent",
            "accounting.Account",
            "acct_assets_group",
        ),
        add_link(
            "accounting.Account",
            "acct_revenue",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.FiscalPeriod",
            "fp_2026_11",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.FiscalPeriod",
            "fp_2026_12",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.MonetaryAccount",
            "ma_operating_usd",
            "owner",
            "accounting.Counterparty",
            "cp_company",
        ),
        add_link(
            "accounting.MonetaryAccount",
            "ma_operating_usd",
            "custodian",
            "accounting.Counterparty",
            "cp_bank",
        ),
        add_link(
            "accounting.MonetaryAccount",
            "ma_operating_usd",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.MonetaryAccount",
            "ma_operating_usd",
            "gl_account",
            "accounting.Account",
            "acct_cash",
        ),
        add_link(
            "accounting.FinancialEvent",
            "fe_sale_001",
            "provider",
            "accounting.Counterparty",
            "cp_customer",
        ),
        add_link(
            "accounting.FinancialEvent",
            "fe_sale_001",
            "receiver",
            "accounting.Counterparty",
            "cp_company",
        ),
        add_link(
            "accounting.FinancialEvent",
            "fe_sale_001",
            "resource",
            "accounting.MonetaryAccount",
            "ma_operating_usd",
        ),
    ]
}

#[allow(clippy::expect_used, reason = "accounting test fixture seeder")]
fn seed_base_ledger(cell: &mut Cell) {
    let mut seed = CellCommit::empty();
    seed.writes.extend(base_book_and_counterparty_facts());
    seed.writes.extend(base_account_facts());
    seed.writes.extend(base_period_and_resource_facts());
    seed.links.extend(base_ledger_links());
    cell.commit(seed)
        .expect("base accounting ledger graph commits");
}

fn assert_invalid_fiscal_periods_refused(cell: &mut Cell) {
    for (period_id, starts_at, ends_at) in [
        (
            "fp_negative",
            "2026-12-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
        ),
        ("fp_zero", "2026-11-01T00:00:00Z", "2026-11-01T00:00:00Z"),
    ] {
        let mut invalid_period = CellCommit::empty();
        invalid_period.writes.push(put_fact(
            "accounting.FiscalPeriod",
            period_id,
            &json!({
                "id": period_id,
                "period_code": period_id,
                "starts_at": starts_at,
                "ends_at": ends_at,
                "timezone": "UTC",
                "status": "open"
            }),
        ));
        invalid_period.links.push(add_link(
            "accounting.FiscalPeriod",
            period_id,
            "book",
            "accounting.Book",
            "book_main",
        ));
        assert_time_interval_refusal(cell.commit(invalid_period));
    }
}

fn assert_account_and_monetary_laws_enforced(cell: &mut Cell) {
    let mut bad_group_account = CellCommit::empty();
    bad_group_account.writes.push(put_fact(
        "accounting.Account",
        "acct_bad_group",
        &json!({
            "id": "acct_bad_group",
            "account_code": "1999",
            "display_name": "Invalid Direct-Posting Group",
            "account_class": "asset",
            "normal_balance": "debit",
            "currency": "USD",
            "is_group": true,
            "allow_direct_posting": true,
            "status": "active"
        }),
    ));
    bad_group_account.links.push(add_link(
        "accounting.Account",
        "acct_bad_group",
        "book",
        "accounting.Book",
        "book_main",
    ));
    assert_law_refusal(
        cell.commit(bad_group_account),
        "accounting.Account.group_no_direct_posting",
    );

    let mut negative_cash = CellCommit::empty();
    negative_cash.writes.push(put_fact(
        "accounting.MonetaryAccount",
        "ma_negative",
        &json!({
            "id": "ma_negative",
            "resource_code": "CASH-NEG",
            "display_name": "Overdrawn Cash",
            "quantity": -50,
            "unit": null,
            "currency": "USD",
            "valuation_method": null,
            "account_kind": "cash"
        }),
    ));
    negative_cash.links.extend([
        add_link(
            "accounting.MonetaryAccount",
            "ma_negative",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.MonetaryAccount",
            "ma_negative",
            "gl_account",
            "accounting.Account",
            "acct_cash",
        ),
    ]);
    assert_law_refusal(
        cell.commit(negative_cash),
        "accounting.MonetaryAccount.non_negative",
    );
}

fn journal_entry_fact(
    id: &str,
    entry_number: &str,
    total_debit: &Value,
    total_credit: &Value,
    created_by: &str,
    memo: &str,
) -> FactWrite {
    put_fact(
        "accounting.JournalEntry",
        id,
        &json!({
            "id": id,
            "entry_number": entry_number,
            "posting_date": "2026-11-15",
            "currency": "USD",
            "total_debit": total_debit,
            "total_credit": total_credit,
            "memo": memo,
            "created_by_user_id": created_by,
            "status": "draft"
        }),
    )
}

fn journal_line_fact(
    id: &str,
    account_code: &str,
    side: &str,
    amount: &Value,
    debit_amount: &Value,
    credit_amount: &Value,
    description: &str,
) -> FactWrite {
    put_fact(
        "accounting.JournalLine",
        id,
        &json!({
            "id": id,
            "account_code": account_code,
            "side": side,
            "amount": amount,
            "currency": "USD",
            "debit_amount": debit_amount,
            "credit_amount": credit_amount,
            "description": description
        }),
    )
}

fn draft_journal_entry_facts() -> Vec<FactWrite> {
    vec![
        journal_entry_fact(
            "je_balanced",
            "JE-2026-0001",
            &json!(140.73),
            &json!(140.73),
            "principal:senior_accountant",
            "Record fractional cash sale",
        ),
        journal_entry_fact(
            "je_second_balanced",
            "JE-2026-0002",
            &json!(500),
            &json!(500),
            "principal:accountant",
            "Late period posting attempt",
        ),
        journal_entry_fact(
            "je_unbalanced",
            "JE-2026-0003",
            &json!(1000),
            &json!(900),
            "principal:accountant",
            "Unbalanced draft",
        ),
        journal_entry_fact(
            "je_no_lines",
            "JE-2026-0004",
            &json!(200),
            &json!(200),
            "principal:accountant",
            "Balanced header with zero linked lines",
        ),
        journal_entry_fact(
            "je_line_mismatch",
            "JE-2026-0005",
            &json!(1000),
            &json!(1000),
            "principal:accountant",
            "Balanced header whose lines do not reconcile",
        ),
    ]
}

fn draft_journal_line_facts() -> Vec<FactWrite> {
    vec![
        journal_line_fact(
            "jl_debit_1",
            "1010",
            "debit",
            &json!(140.73),
            &json!(140.73),
            &json!(0),
            "Debit operating cash",
        ),
        journal_line_fact(
            "jl_credit_1",
            "4010",
            "credit",
            &json!(-140.73),
            &json!(0),
            &json!(140.73),
            "Credit product revenue (negative signed core.LedgerEffect amount)",
        ),
        journal_line_fact(
            "jl_second_debit",
            "1010",
            "debit",
            &json!(500),
            &json!(500),
            &json!(0),
            "Debit operating cash for second entry",
        ),
        journal_line_fact(
            "jl_second_credit",
            "4010",
            "credit",
            &json!(-500),
            &json!(0),
            &json!(500),
            "Credit product revenue for second entry",
        ),
        journal_line_fact(
            "jl_mismatch_debit",
            "1010",
            "debit",
            &json!(600),
            &json!(600),
            &json!(0),
            "Under-stated debit line (600 vs 1000 header)",
        ),
        journal_line_fact(
            "jl_mismatch_credit",
            "4010",
            "credit",
            &json!(-1000),
            &json!(0),
            &json!(1000),
            "Credit line matching header credit",
        ),
    ]
}

#[allow(clippy::expect_used, reason = "accounting test fixture seeder")]
fn seed_journal_entries_and_lines(cell: &mut Cell) {
    let mut journal_seed = CellCommit::empty();
    journal_seed.writes.extend(draft_journal_entry_facts());
    journal_seed.writes.extend(draft_journal_line_facts());
    for entry_id in [
        "je_balanced",
        "je_second_balanced",
        "je_unbalanced",
        "je_no_lines",
        "je_line_mismatch",
    ] {
        journal_seed.links.extend([
            add_link(
                "accounting.JournalEntry",
                entry_id,
                "book",
                "accounting.Book",
                "book_main",
            ),
            add_link(
                "accounting.JournalEntry",
                entry_id,
                "period",
                "accounting.FiscalPeriod",
                "fp_2026_11",
            ),
        ]);
    }
    for (line_id, entry_id, account_id) in [
        ("jl_debit_1", "je_balanced", "acct_cash"),
        ("jl_credit_1", "je_balanced", "acct_revenue"),
        ("jl_second_debit", "je_second_balanced", "acct_cash"),
        ("jl_second_credit", "je_second_balanced", "acct_revenue"),
        ("jl_mismatch_debit", "je_line_mismatch", "acct_cash"),
        ("jl_mismatch_credit", "je_line_mismatch", "acct_revenue"),
    ] {
        journal_seed.links.extend([
            add_link(
                "accounting.JournalLine",
                line_id,
                "event",
                "accounting.FinancialEvent",
                "fe_sale_001",
            ),
            add_link(
                "accounting.JournalLine",
                line_id,
                "entry",
                "accounting.JournalEntry",
                entry_id,
            ),
            add_link(
                "accounting.JournalLine",
                line_id,
                "account",
                "accounting.Account",
                account_id,
            ),
        ]);
    }
    cell.commit(journal_seed)
        .expect("draft journal entries and valid one-sided lines commit");
}

fn assert_invalid_journal_lines_and_direct_posted_refused(cell: &mut Cell) {
    for (line_id, fact, account_id) in [
        (
            "jl_two_sided",
            journal_line_fact(
                "jl_two_sided",
                "1010",
                "debit",
                &json!(500),
                &json!(500),
                &json!(100),
                "Invalid two-sided posting line",
            ),
            "acct_cash",
        ),
        (
            "jl_positive_credit",
            journal_line_fact(
                "jl_positive_credit",
                "4010",
                "credit",
                &json!(100),
                &json!(0),
                &json!(100),
                "Credit line violating core.LedgerEffect negative sign convention",
            ),
            "acct_revenue",
        ),
    ] {
        let mut commit = CellCommit::empty();
        commit.writes.push(fact);
        commit.links.extend([
            add_link(
                "accounting.JournalLine",
                line_id,
                "event",
                "accounting.FinancialEvent",
                "fe_sale_001",
            ),
            add_link(
                "accounting.JournalLine",
                line_id,
                "entry",
                "accounting.JournalEntry",
                "je_balanced",
            ),
            add_link(
                "accounting.JournalLine",
                line_id,
                "account",
                "accounting.Account",
                account_id,
            ),
        ]);
        assert_law_refusal(cell.commit(commit), "accounting.JournalLine.one_sided");
    }

    let mut posted_unbalanced = CellCommit::empty();
    posted_unbalanced.writes.push(put_fact(
        "accounting.JournalEntry",
        "je_posted_unbalanced",
        &json!({
            "id": "je_posted_unbalanced",
            "entry_number": "JE-2026-BAD",
            "posting_date": "2026-11-15",
            "currency": "USD",
            "total_debit": 1000,
            "total_credit": 900,
            "memo": "Direct posted unbalanced attempt",
            "created_by_user_id": "principal:accountant",
            "status": "posted"
        }),
    ));
    posted_unbalanced.links.extend([
        add_link(
            "accounting.JournalEntry",
            "je_posted_unbalanced",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_posted_unbalanced",
            "period",
            "accounting.FiscalPeriod",
            "fp_2026_11",
        ),
    ]);
    assert_law_refusal(
        cell.commit(posted_unbalanced),
        "accounting.JournalEntry.balanced",
    );
}

#[allow(clippy::expect_used, reason = "accounting E2E journal posting helper")]
fn assert_journal_post_refusals(fixture: &AccountingFixture, cell: &mut Cell) {
    let unbalanced_post = fixture
        .invoke(
            cell,
            "accounting.journal.post",
            &json!({"entryId": "je_unbalanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-unbalanced",
        )
        .expect_err("unbalanced journal entry must be refused");
    assert_eq!(unbalanced_post.code(), "action.precondition_failed");
    assert_eq!(
        status_of(cell, "accounting.JournalEntry", "je_unbalanced"),
        "draft"
    );

    // Zero linked JournalLines and mismatched line sums are both refused at commit
    // by LAW-LINK-MANY-1 law accounting.JournalEntry.balanced.
    for (entry_id, idem_key) in [
        ("je_no_lines", "je-post-no-lines"),
        ("je_line_mismatch", "je-post-line-mismatch"),
    ] {
        assert_action_law_refusal(
            fixture.invoke(
                cell,
                "accounting.journal.post",
                &json!({"entryId": entry_id, "periodId": "fp_2026_11"}),
                "principal:controller",
                idem_key,
            ),
            "accounting.JournalEntry.balanced",
        );
        assert_eq!(
            status_of(cell, "accounting.JournalEntry", entry_id),
            "draft"
        );
    }
}

#[allow(clippy::expect_used, reason = "accounting E2E journal SoD helper")]
fn assert_journal_sod_post_and_reverse(fixture: &AccountingFixture, cell: &mut Cell) {
    // Fail-closed Cedar AccessIR + Separation of Duties on accounting.journal.post.
    for (actor, key) in [
        ("principal:viewer", "je-post-viewer-denied"),
        ("principal:accountant", "je-post-accountant-denied"),
        ("principal:senior_accountant", "je-post-creator-sod-denied"),
    ] {
        let denied = fixture
            .invoke(
                cell,
                "accounting.journal.post",
                &json!({"entryId": "je_balanced", "periodId": "fp_2026_11"}),
                actor,
                key,
            )
            .expect_err("unauthorized or self-posting actor must be denied");
        assert_eq!(denied.code(), "action.permission_denied");
        assert_eq!(
            status_of(cell, "accounting.JournalEntry", "je_balanced"),
            "draft"
        );
    }

    fixture
        .invoke(
            cell,
            "accounting.journal.post",
            &json!({"entryId": "je_balanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-controller-ok",
        )
        .expect("controller posts balanced draft created by senior_accountant");
    assert_eq!(
        status_of(cell, "accounting.JournalEntry", "je_balanced"),
        "posted"
    );

    let reverse_denied = fixture
        .invoke(
            cell,
            "accounting.journal.reverse",
            &json!({"entryId": "je_balanced"}),
            "principal:senior_accountant",
            "je-reverse-senior-denied",
        )
        .expect_err("senior_accountant cannot reverse posted entry");
    assert_eq!(reverse_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            cell,
            "accounting.journal.reverse",
            &json!({"entryId": "je_balanced"}),
            "principal:controller",
            "je-reverse-controller-ok",
        )
        .expect("controller reverses posted entry");
    assert_eq!(
        status_of(cell, "accounting.JournalEntry", "je_balanced"),
        "reversed"
    );
}

#[allow(clippy::expect_used, reason = "accounting E2E fiscal period helper")]
fn assert_fiscal_period_close_lock_and_mismatched_period_enforced(
    fixture: &AccountingFixture,
    cell: &mut Cell,
) {
    let close_denied = fixture
        .invoke(
            cell,
            "accounting.period.close",
            &json!({"periodId": "fp_2026_11"}),
            "principal:accountant",
            "period-close-accountant-denied",
        )
        .expect_err("accountant cannot close fiscal period");
    assert_eq!(close_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            cell,
            "accounting.period.close",
            &json!({"periodId": "fp_2026_11"}),
            "principal:senior_accountant",
            "period-close-senior-ok",
        )
        .expect("senior_accountant closes open period");
    assert_eq!(
        status_of(cell, "accounting.FiscalPeriod", "fp_2026_11"),
        "closed"
    );

    // Closed period and mismatched open periodId are both refused by precondition.
    for (period_id, idem_key) in [
        ("fp_2026_11", "je-post-closed-period-refused"),
        ("fp_2026_12", "je-post-mismatched-open-period-refused"),
    ] {
        let refused = fixture
            .invoke(
                cell,
                "accounting.journal.post",
                &json!({"entryId": "je_second_balanced", "periodId": period_id}),
                "principal:controller",
                idem_key,
            )
            .expect_err("posting into closed or mismatched period must be refused");
        assert_eq!(refused.code(), "action.precondition_failed");
        assert_eq!(
            status_of(cell, "accounting.JournalEntry", "je_second_balanced"),
            "draft"
        );
    }

    let lock_denied = fixture
        .invoke(
            cell,
            "accounting.period.lock",
            &json!({"periodId": "fp_2026_11"}),
            "principal:senior_accountant",
            "period-lock-senior-denied",
        )
        .expect_err("senior_accountant cannot lock fiscal period");
    assert_eq!(lock_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            cell,
            "accounting.period.lock",
            &json!({"periodId": "fp_2026_11"}),
            "principal:controller",
            "period-lock-controller-ok",
        )
        .expect("controller locks closed period");
    assert_eq!(
        status_of(cell, "accounting.FiscalPeriod", "fp_2026_11"),
        "locked"
    );

    let post_locked = fixture
        .invoke(
            cell,
            "accounting.journal.post",
            &json!({"entryId": "je_second_balanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-locked-period-refused",
        )
        .expect_err("posting into a locked fiscal period must be refused");
    assert_eq!(post_locked.code(), "action.precondition_failed");
    assert_eq!(
        status_of(cell, "accounting.JournalEntry", "je_second_balanced"),
        "draft"
    );
}

fn invoice_fact(
    id: &str,
    invoice_number: &str,
    subtotal: &Value,
    tax_amount: &Value,
    amount: &Value,
    amount_paid: &Value,
    status: &str,
) -> FactWrite {
    put_fact(
        "accounting.Invoice",
        id,
        &json!({
            "id": id,
            "invoice_number": invoice_number,
            "direction": "receivable",
            "subtotal": subtotal,
            "tax_amount": tax_amount,
            "amount": amount,
            "amount_paid": amount_paid,
            "currency": "USD",
            "due_date": "2026-12-15",
            "status": status
        }),
    )
}

fn invoice_links(invoice_id: &str) -> [LinkWrite; 3] {
    [
        add_link(
            "accounting.Invoice",
            invoice_id,
            "creditor",
            "accounting.Counterparty",
            "cp_company",
        ),
        add_link(
            "accounting.Invoice",
            invoice_id,
            "debtor",
            "accounting.Counterparty",
            "cp_customer",
        ),
        add_link(
            "accounting.Invoice",
            invoice_id,
            "book",
            "accounting.Book",
            "book_main",
        ),
    ]
}

#[allow(clippy::expect_used, reason = "accounting test fixture seeder")]
fn seed_test_invoices(cell: &mut Cell) {
    let mut mismatched = CellCommit::empty();
    mismatched.writes.push(invoice_fact(
        "inv_bad_sum",
        "INV-2026-BAD1",
        &json!(123.45),
        &json!(17.28),
        &json!(140.74),
        &json!(0),
        "draft",
    ));
    mismatched.links.extend(invoice_links("inv_bad_sum"));
    assert_law_refusal(
        cell.commit(mismatched),
        "accounting.Invoice.amount_conservation",
    );

    let mut invoice_seed = CellCommit::empty();
    invoice_seed.writes.extend([
        invoice_fact(
            "inv_valid",
            "INV-2026-0001",
            &json!(123.45),
            &json!(17.28),
            &json!(140.73),
            &json!(0),
            "draft",
        ),
        invoice_fact(
            "inv_zero_draft",
            "INV-2026-0002",
            &json!(0),
            &json!(0),
            &json!(0),
            &json!(0),
            "draft",
        ),
        invoice_fact(
            "inv_to_void",
            "INV-2026-0003",
            &json!(200),
            &json!(0),
            &json!(200),
            &json!(0),
            "draft",
        ),
        invoice_fact(
            "inv_still_draft",
            "INV-2026-0004",
            &json!(250),
            &json!(0),
            &json!(250),
            &json!(0),
            "draft",
        ),
        invoice_fact(
            "inv_partially_paid",
            "INV-2026-0005",
            &json!(100),
            &json!(0),
            &json!(100),
            &json!(50.25),
            "partially_paid",
        ),
    ]);
    for invoice_id in [
        "inv_valid",
        "inv_zero_draft",
        "inv_to_void",
        "inv_still_draft",
        "inv_partially_paid",
    ] {
        invoice_seed.links.extend(invoice_links(invoice_id));
    }
    cell.commit(invoice_seed).expect("draft invoices commit");
}

#[allow(clippy::expect_used, reason = "accounting E2E invoice helper")]
fn assert_invoice_issue_and_void_guards(fixture: &AccountingFixture, cell: &mut Cell) {
    let zero_issue = fixture
        .invoke(
            cell,
            "accounting.invoice.issue",
            &json!({"invoiceId": "inv_zero_draft"}),
            "principal:accountant",
            "inv-issue-zero-refused",
        )
        .expect_err("zero-amount draft invoice cannot be issued");
    assert_eq!(zero_issue.code(), "action.precondition_failed");

    fixture
        .invoke(
            cell,
            "accounting.invoice.issue",
            &json!({"invoiceId": "inv_valid"}),
            "principal:accountant",
            "inv-issue-valid",
        )
        .expect("accountant issues positive fractional draft invoice");
    assert_eq!(status_of(cell, "accounting.Invoice", "inv_valid"), "issued");

    let void_denied = fixture
        .invoke(
            cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_to_void"}),
            "principal:accountant",
            "inv-void-accountant-denied",
        )
        .expect_err("accountant cannot void invoice");
    assert_eq!(void_denied.code(), "action.permission_denied");

    let void_paid_refused = fixture
        .invoke(
            cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_partially_paid"}),
            "principal:senior_accountant",
            "inv-void-partially-paid-refused",
        )
        .expect_err("partially paid invoice with recorded payments cannot be voided");
    assert_eq!(void_paid_refused.code(), "action.precondition_failed");
    assert_eq!(
        status_of(cell, "accounting.Invoice", "inv_partially_paid"),
        "partially_paid"
    );

    fixture
        .invoke(
            cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_to_void"}),
            "principal:senior_accountant",
            "inv-void-senior-ok",
        )
        .expect("senior_accountant voids unpaid draft invoice");
    assert_eq!(
        status_of(cell, "accounting.Invoice", "inv_to_void"),
        "voided"
    );
}

#[allow(
    clippy::expect_used,
    reason = "accounting E2E payment precondition helper"
)]
fn assert_invalid_payment_preconditions_refused(fixture: &AccountingFixture, cell: &mut Cell) {
    for (payment_id, invoice_id, quantity, unit, currency, idem_key) in [
        ("pay_zero", "inv_valid", "0", None, Some("USD"), "pay-zero"),
        (
            "pay_non_monetary",
            "inv_valid",
            "10",
            Some("EA"),
            Some("USD"),
            "pay-non-monetary",
        ),
        (
            "pay_draft",
            "inv_still_draft",
            "50",
            None,
            Some("USD"),
            "pay-draft",
        ),
        (
            "pay_voided",
            "inv_to_void",
            "50",
            None,
            Some("USD"),
            "pay-voided",
        ),
        (
            "pay_wrong_ccy",
            "inv_valid",
            "100",
            None,
            Some("EUR"),
            "pay-wrong-ccy",
        ),
        (
            "pay_overpay",
            "inv_valid",
            "140.74",
            None,
            Some("USD"),
            "pay-overpay",
        ),
    ] {
        let invalid_payment = json!({
            "id": payment_id,
            "action": "transfer_ownership",
            "occurred_at": "2026-11-20T10:00:00Z",
            "quantity": quantity,
            "unit": unit,
            "currency": currency,
            "payment_method": "bank_transfer",
            "reference_no": format!("REF-{payment_id}"),
            "providerId": "cp_customer",
            "receiverId": "cp_company",
            "resourceId": "ma_operating_usd",
            "invoiceId": invoice_id
        });
        let refused = fixture
            .invoke(
                cell,
                "accounting.payment.record",
                &invalid_payment,
                "principal:accountant",
                idem_key,
            )
            .expect_err("invalid settlement payment must be refused");
        assert_eq!(
            refused.code(),
            "action.precondition_failed",
            "expected precondition_failed for {payment_id}"
        );
    }
}

#[allow(
    clippy::expect_used,
    reason = "accounting E2E payment create_only helper"
)]
fn assert_payment_create_only_and_replay(fixture: &AccountingFixture, cell: &mut Cell) {
    let valid_payment = json!({
        "id": "pay_001",
        "action": "transfer_ownership",
        "occurred_at": "2026-11-20T10:15:00Z",
        "quantity": "140.73",
        "unit": null,
        "currency": "USD",
        "payment_method": "bank_transfer",
        "reference_no": "WIRE-2026-0001",
        "providerId": "cp_customer",
        "receiverId": "cp_company",
        "resourceId": "ma_operating_usd",
        "invoiceId": "inv_valid"
    });
    fixture
        .invoke(
            cell,
            "accounting.payment.record",
            &valid_payment,
            "principal:accountant",
            "pay-record-1",
        )
        .expect("create-only payment commits");
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new("accounting.Payment", "pay_001"),
            "settles",
            FactKey::new("accounting.Invoice", "inv_valid"),
        ))
        .expect("settles link loads")
        .exists()
    );
    assert_eq!(status_of(cell, "accounting.Payment", "pay_001"), "recorded");

    let replayed = fixture
        .invoke(
            cell,
            "accounting.payment.record",
            &valid_payment,
            "principal:accountant",
            "pay-record-1",
        )
        .expect("exact retry replays");
    assert!(matches!(replayed, ActionOutcome::Replayed { .. }));

    let duplicate_refused = fixture
        .invoke(
            cell,
            "accounting.payment.record",
            &valid_payment,
            "principal:accountant",
            "pay-record-duplicate-id",
        )
        .expect_err("duplicate Payment ID must be refused by create_only write mode");
    assert_eq!(duplicate_refused.code(), "action.write_mode_failed");
}

#[allow(
    clippy::expect_used,
    reason = "accounting E2E payment immutable lifecycle helper"
)]
fn assert_payment_immutable_lifecycle_refuses_set(fixture: &AccountingFixture, cell: &Cell) {
    let payment_key = FactKey::new("accounting.Payment", "pay_001");
    let payment_state = cell.load(&payment_key).expect("recorded payment loads");
    let payment_slice = FactSlice::from([(payment_key.clone(), payment_state.clone())]);
    let mutation_output = DecisionOutput {
        facts: vec![FactChange::Set {
            object_type: ObjectTypeName("accounting.Payment".into()),
            key: "pay_001".into(),
            property: PropertyId("reference_no".into()),
            value: RuntimeValue::from("TAMPERED-WIRE"),
        }],
        commands: vec![],
        timers: vec![],
    };
    let mutation_turn = DecisionTurn {
        id: "turn:payment-immutable-mutation".to_owned(),
        now: LogicalTime::from_millis(KNOWN_AT),
        world: World::Main,
        consumes: None,
    };
    let immutable_err = prepare_decision::<std::convert::Infallible>(
        &fixture.catalog(),
        &payment_slice,
        &mutation_turn,
        &mutation_output,
    )
    .expect_err("property mutation on recorded Payment must be refused by immutable lifecycle");
    assert_eq!(immutable_err.code(), lifecycle_codes::IMMUTABLE_STATE_WRITE);
    assert!(matches!(
        immutable_err,
        DecisionRefusal::ImmutableStateWrite {
            ref lifecycle,
            ref state,
            ..
        } if lifecycle == "status" && state == "recorded"
    ));
    assert_eq!(
        cell.load(&payment_key)
            .expect("payment reloads unchanged")
            .version,
        payment_state.version
    );
}

fn assert_void_with_linked_payment_refused(fixture: &AccountingFixture, cell: &mut Cell) {
    // Voiding issued inv_valid must refuse once pay_001 links to it via settles,
    // even while inv_valid.amount_paid remains 0 (LAW-LINK-MANY-1).
    assert_action_law_refusal(
        fixture.invoke(
            cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_valid"}),
            "principal:senior_accountant",
            "inv-void-with-linked-payment-refused",
        ),
        "accounting.Invoice.no_void_with_payments",
    );
    assert_eq!(status_of(cell, "accounting.Invoice", "inv_valid"), "issued");

    // Attaching a Payment directly to an already-voided invoice is also refused.
    let mut payment_on_voided = CellCommit::empty();
    payment_on_voided.writes.push(put_fact(
        "accounting.Payment",
        "pay_on_voided_direct",
        &json!({
            "id": "pay_on_voided_direct",
            "action": "transfer_ownership",
            "occurred_at": "2026-11-20T11:00:00Z",
            "quantity": 25,
            "unit": null,
            "currency": "USD",
            "payment_method": "bank_transfer",
            "reference_no": "WIRE-BAD-VOIDED",
            "status": "recorded"
        }),
    ));
    payment_on_voided.links.extend([
        add_link(
            "accounting.Payment",
            "pay_on_voided_direct",
            "provider",
            "accounting.Counterparty",
            "cp_customer",
        ),
        add_link(
            "accounting.Payment",
            "pay_on_voided_direct",
            "receiver",
            "accounting.Counterparty",
            "cp_company",
        ),
        add_link(
            "accounting.Payment",
            "pay_on_voided_direct",
            "resource",
            "accounting.MonetaryAccount",
            "ma_operating_usd",
        ),
        add_link(
            "accounting.Payment",
            "pay_on_voided_direct",
            "settles",
            "accounting.Invoice",
            "inv_to_void",
        ),
    ]);
    assert_law_refusal(
        cell.commit(payment_on_voided),
        "accounting.Invoice.no_void_with_payments",
    );
}

#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "accounting E2E durability reopen helper"
)]
fn assert_reopen_preserves_dump_and_ledger(fixture: &AccountingFixture, cell: Cell, path: &Path) {
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
    .expect("reopened accounting cell");
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
#[allow(clippy::expect_used, reason = "accounting E2E admission test")]
fn accounting_pack_admits_against_core_base_and_installs_with_receipt() {
    let base = decode_envelopes(&load_core_base_values());
    let artifacts = decode_envelopes(&load_accounting_pack_values());

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
        "accounting pack must admit cleanly against Core base: {errors:#?}"
    );

    let law_ids: BTreeSet<&str> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    for expected_law in [
        "accounting.JournalEntry.balanced.law",
        "accounting.JournalLine.one_sided.law",
        "accounting.Account.group_no_direct_posting.law",
        "accounting.Invoice.amount_conservation.law",
        "accounting.Invoice.no_void_with_payments.law",
        "accounting.Payment.positive_amount.law",
        "accounting.MonetaryAccount.non_negative.law",
    ] {
        assert!(
            law_ids.contains(expected_law),
            "expected law {expected_law} in admitted accounting pack: {law_ids:?}"
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
        "no checker findings against accounting laws: {law_findings:#?}"
    );

    let fixture = AccountingFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("accounting-install.sqlite");
    let cell = fixture.open_installed_cell(&path);
    cell.store()
        .read()
        .verify_ledger()
        .expect("installed accounting cell ledger verifies");
}

#[test]
#[allow(clippy::expect_used, reason = "accounting E2E ledger test")]
fn accounting_ledger_laws_periods_and_separation_of_duties_enforce_in_real_cell() {
    let fixture = AccountingFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("accounting-ledger.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_base_ledger(&mut cell);

    assert_invalid_fiscal_periods_refused(&mut cell);
    assert_account_and_monetary_laws_enforced(&mut cell);
    seed_journal_entries_and_lines(&mut cell);
    assert_invalid_journal_lines_and_direct_posted_refused(&mut cell);
    assert_journal_post_refusals(&fixture, &mut cell);
    assert_journal_sod_post_and_reverse(&fixture, &mut cell);
    assert_fiscal_period_close_lock_and_mismatched_period_enforced(&fixture, &mut cell);

    cell.store()
        .read()
        .verify_ledger()
        .expect("ledger hash chain verifies");
}

#[test]
#[allow(clippy::expect_used, reason = "accounting E2E settlement test")]
fn accounting_invoice_conservation_and_create_only_payment_enforce_in_real_cell() {
    let fixture = AccountingFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("accounting-settlement.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_base_ledger(&mut cell);

    seed_test_invoices(&mut cell);
    assert_invoice_issue_and_void_guards(&fixture, &mut cell);
    assert_invalid_payment_preconditions_refused(&fixture, &mut cell);
    assert_payment_create_only_and_replay(&fixture, &mut cell);
    assert_payment_immutable_lifecycle_refuses_set(&fixture, &cell);
    assert_void_with_linked_payment_refused(&fixture, &mut cell);

    assert_reopen_preserves_dump_and_ledger(&fixture, cell, &path);
}
