#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "Accounting vertical end-to-end kernel execution fixtures"
)]
//! End-to-end `norn-cell` execution suite for the jurisdiction-agnostic
//! Accounting Domain Pack (`registry/packs/accounting/`): static admission
//! against the Core REA + `core.TimeInterval` base, real SQLite cell definition
//! installation with checker receipt, double-entry journal & fiscal period
//! governance, invoice conservation, create-only payment immutability, and
//! fail-closed Cedar `AccessIR` with separation of duties on journal posting.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::{InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_checker::{
    EntityArtifactSetContext, ReceiptSources, admission_receipt_sha256, check_artifact_set,
    check_artifact_set_with_receipt,
};
use norn_ir::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactKind, InterfaceDef, ObjectTypeDef,
    PropertyTypeDef, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite, LogicalTime, TypeCatalog, World, commit_action,
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
        let principals =
            AccountingPrincipals::new(&types, &interfaces, &properties, &actions, &policies);

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

fn seed_base_ledger(cell: &mut Cell) {
    let mut seed = CellCommit::empty();
    seed.writes.extend([
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
    ]);
    seed.links.extend([
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
    ]);
    cell.commit(seed)
        .expect("base accounting ledger graph commits");
}

#[test]
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
fn accounting_ledger_laws_periods_and_separation_of_duties_enforce_in_real_cell() {
    let fixture = AccountingFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("accounting-ledger.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_base_ledger(&mut cell);

    // 1. core.TimeInterval invariant refuses negative-duration and zero-duration FiscalPeriods.
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
        assert!(
            cell.commit(invalid_period).is_err(),
            "FiscalPeriod {period_id} with invalid interval must be refused by core.TimeInterval"
        );
    }

    // 2. Group account allowing direct posting is refused by accounting.Account.group_no_direct_posting.
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
    assert!(
        cell.commit(bad_group_account).is_err(),
        "group account with allow_direct_posting=true must be refused"
    );

    // 3. Negative MonetaryAccount balance is refused by accounting.MonetaryAccount.non_negative.
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
    assert!(
        cell.commit(negative_cash).is_err(),
        "negative MonetaryAccount quantity must be refused"
    );

    // 4. Seed balanced and unbalanced draft JournalEntries plus valid one-sided lines.
    let mut journal_seed = CellCommit::empty();
    journal_seed.writes.extend([
        put_fact(
            "accounting.JournalEntry",
            "je_balanced",
            &json!({
                "id": "je_balanced",
                "entry_number": "JE-2026-0001",
                "posting_date": "2026-11-15",
                "currency": "USD",
                "total_debit": 1000,
                "total_credit": 1000,
                "memo": "Record cash sale",
                "created_by_user_id": "principal:senior_accountant",
                "status": "draft"
            }),
        ),
        put_fact(
            "accounting.JournalEntry",
            "je_second_balanced",
            &json!({
                "id": "je_second_balanced",
                "entry_number": "JE-2026-0002",
                "posting_date": "2026-11-16",
                "currency": "USD",
                "total_debit": 500,
                "total_credit": 500,
                "memo": "Late period posting attempt",
                "created_by_user_id": "principal:accountant",
                "status": "draft"
            }),
        ),
        put_fact(
            "accounting.JournalEntry",
            "je_unbalanced",
            &json!({
                "id": "je_unbalanced",
                "entry_number": "JE-2026-0003",
                "posting_date": "2026-11-15",
                "currency": "USD",
                "total_debit": 1000,
                "total_credit": 900,
                "memo": "Unbalanced draft",
                "created_by_user_id": "principal:accountant",
                "status": "draft"
            }),
        ),
        put_fact(
            "accounting.JournalLine",
            "jl_debit_1",
            &json!({
                "id": "jl_debit_1",
                "account_code": "1010",
                "side": "debit",
                "amount": 1000,
                "currency": "USD",
                "debit_amount": 1000,
                "credit_amount": 0,
                "description": "Debit operating cash"
            }),
        ),
        put_fact(
            "accounting.JournalLine",
            "jl_credit_1",
            &json!({
                "id": "jl_credit_1",
                "account_code": "4010",
                "side": "credit",
                "amount": 1000,
                "currency": "USD",
                "debit_amount": 0,
                "credit_amount": 1000,
                "description": "Credit product revenue"
            }),
        ),
    ]);
    journal_seed.links.extend([
        add_link(
            "accounting.JournalEntry",
            "je_balanced",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_balanced",
            "period",
            "accounting.FiscalPeriod",
            "fp_2026_11",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_second_balanced",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_second_balanced",
            "period",
            "accounting.FiscalPeriod",
            "fp_2026_11",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_unbalanced",
            "book",
            "accounting.Book",
            "book_main",
        ),
        add_link(
            "accounting.JournalEntry",
            "je_unbalanced",
            "period",
            "accounting.FiscalPeriod",
            "fp_2026_11",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_debit_1",
            "event",
            "accounting.FinancialEvent",
            "fe_sale_001",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_debit_1",
            "entry",
            "accounting.JournalEntry",
            "je_balanced",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_debit_1",
            "account",
            "accounting.Account",
            "acct_cash",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_credit_1",
            "event",
            "accounting.FinancialEvent",
            "fe_sale_001",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_credit_1",
            "entry",
            "accounting.JournalEntry",
            "je_balanced",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_credit_1",
            "account",
            "accounting.Account",
            "acct_revenue",
        ),
    ]);
    cell.commit(journal_seed)
        .expect("draft journal entries and valid one-sided lines commit");

    // 5. Two-sided JournalLine is refused by accounting.JournalLine.one_sided.
    let mut two_sided_line = CellCommit::empty();
    two_sided_line.writes.push(put_fact(
        "accounting.JournalLine",
        "jl_two_sided",
        &json!({
            "id": "jl_two_sided",
            "account_code": "1010",
            "side": "debit",
            "amount": 500,
            "currency": "USD",
            "debit_amount": 500,
            "credit_amount": 100,
            "description": "Invalid two-sided posting line"
        }),
    ));
    two_sided_line.links.extend([
        add_link(
            "accounting.JournalLine",
            "jl_two_sided",
            "event",
            "accounting.FinancialEvent",
            "fe_sale_001",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_two_sided",
            "entry",
            "accounting.JournalEntry",
            "je_balanced",
        ),
        add_link(
            "accounting.JournalLine",
            "jl_two_sided",
            "account",
            "accounting.Account",
            "acct_cash",
        ),
    ]);
    assert!(
        cell.commit(two_sided_line).is_err(),
        "two-sided JournalLine must be refused by accounting.JournalLine.one_sided.law"
    );

    // 6. Direct commit of a posted unbalanced JournalEntry is refused by accounting.JournalEntry.balanced.
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
    assert!(
        cell.commit(posted_unbalanced).is_err(),
        "posted unbalanced JournalEntry must be refused by accounting.JournalEntry.balanced.law"
    );

    // 7. Action-level refusal of unbalanced JournalEntry via accounting.journal.post.
    let unbalanced_post = fixture
        .invoke(
            &mut cell,
            "accounting.journal.post",
            &json!({"entryId": "je_unbalanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-unbalanced",
        )
        .expect_err("unbalanced journal entry must be refused");
    assert_eq!(unbalanced_post.code(), "action.precondition_failed");
    assert_eq!(
        status_of(&cell, "accounting.JournalEntry", "je_unbalanced"),
        "draft"
    );

    // 8. Fail-closed Cedar AccessIR + Separation of Duties on accounting.journal.post:
    //    - viewer / accountant lack posting role -> denied
    //    - senior_accountant has role, but created je_balanced -> denied (SoD!)
    //    - controller has role and != created_by_user_id -> succeeds!
    for (actor, key) in [
        ("principal:viewer", "je-post-viewer-denied"),
        ("principal:accountant", "je-post-accountant-denied"),
        ("principal:senior_accountant", "je-post-creator-sod-denied"),
    ] {
        let denied = fixture
            .invoke(
                &mut cell,
                "accounting.journal.post",
                &json!({"entryId": "je_balanced", "periodId": "fp_2026_11"}),
                actor,
                key,
            )
            .expect_err("unauthorized or self-posting actor must be denied");
        assert_eq!(denied.code(), "action.permission_denied");
        assert_eq!(
            status_of(&cell, "accounting.JournalEntry", "je_balanced"),
            "draft"
        );
    }

    fixture
        .invoke(
            &mut cell,
            "accounting.journal.post",
            &json!({"entryId": "je_balanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-controller-ok",
        )
        .expect("controller posts balanced draft created by senior_accountant");
    assert_eq!(
        status_of(&cell, "accounting.JournalEntry", "je_balanced"),
        "posted"
    );

    // Only controller/owner may reverse a posted entry.
    let reverse_denied = fixture
        .invoke(
            &mut cell,
            "accounting.journal.reverse",
            &json!({"entryId": "je_balanced"}),
            "principal:senior_accountant",
            "je-reverse-senior-denied",
        )
        .expect_err("senior_accountant cannot reverse posted entry");
    assert_eq!(reverse_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            &mut cell,
            "accounting.journal.reverse",
            &json!({"entryId": "je_balanced"}),
            "principal:controller",
            "je-reverse-controller-ok",
        )
        .expect("controller reverses posted entry");
    assert_eq!(
        status_of(&cell, "accounting.JournalEntry", "je_balanced"),
        "reversed"
    );

    // 9. FiscalPeriod close/lock authority + refusal to post into closed or locked period.
    let close_denied = fixture
        .invoke(
            &mut cell,
            "accounting.period.close",
            &json!({"periodId": "fp_2026_11"}),
            "principal:accountant",
            "period-close-accountant-denied",
        )
        .expect_err("accountant cannot close fiscal period");
    assert_eq!(close_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            &mut cell,
            "accounting.period.close",
            &json!({"periodId": "fp_2026_11"}),
            "principal:senior_accountant",
            "period-close-senior-ok",
        )
        .expect("senior_accountant closes open period");
    assert_eq!(
        status_of(&cell, "accounting.FiscalPeriod", "fp_2026_11"),
        "closed"
    );

    let post_closed = fixture
        .invoke(
            &mut cell,
            "accounting.journal.post",
            &json!({"entryId": "je_second_balanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-closed-period-refused",
        )
        .expect_err("posting into a closed fiscal period must be refused");
    assert_eq!(post_closed.code(), "action.precondition_failed");
    assert_eq!(
        status_of(&cell, "accounting.JournalEntry", "je_second_balanced"),
        "draft"
    );

    let lock_denied = fixture
        .invoke(
            &mut cell,
            "accounting.period.lock",
            &json!({"periodId": "fp_2026_11"}),
            "principal:senior_accountant",
            "period-lock-senior-denied",
        )
        .expect_err("senior_accountant cannot lock fiscal period");
    assert_eq!(lock_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            &mut cell,
            "accounting.period.lock",
            &json!({"periodId": "fp_2026_11"}),
            "principal:controller",
            "period-lock-controller-ok",
        )
        .expect("controller locks closed period");
    assert_eq!(
        status_of(&cell, "accounting.FiscalPeriod", "fp_2026_11"),
        "locked"
    );

    let post_locked = fixture
        .invoke(
            &mut cell,
            "accounting.journal.post",
            &json!({"entryId": "je_second_balanced", "periodId": "fp_2026_11"}),
            "principal:controller",
            "je-post-locked-period-refused",
        )
        .expect_err("posting into a locked fiscal period must be refused");
    assert_eq!(post_locked.code(), "action.precondition_failed");
    assert_eq!(
        status_of(&cell, "accounting.JournalEntry", "je_second_balanced"),
        "draft"
    );

    cell.store()
        .read()
        .verify_ledger()
        .expect("ledger hash chain verifies");
}

#[test]
fn accounting_invoice_conservation_and_create_only_payment_enforce_in_real_cell() {
    let fixture = AccountingFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("accounting-settlement.sqlite");
    let mut cell = fixture.open_installed_cell(&path);
    seed_base_ledger(&mut cell);

    // 1. Invoice violating amount == subtotal + tax_amount is refused at commit.
    let mut mismatched_invoice = CellCommit::empty();
    mismatched_invoice.writes.push(put_fact(
        "accounting.Invoice",
        "inv_bad_sum",
        &json!({
            "id": "inv_bad_sum",
            "invoice_number": "INV-2026-BAD1",
            "direction": "receivable",
            "subtotal": 1000,
            "tax_amount": 150,
            "amount": 1100,
            "amount_paid": 0,
            "currency": "USD",
            "due_date": "2026-12-15",
            "status": "draft"
        }),
    ));
    mismatched_invoice.links.extend([
        add_link(
            "accounting.Invoice",
            "inv_bad_sum",
            "creditor",
            "accounting.Counterparty",
            "cp_company",
        ),
        add_link(
            "accounting.Invoice",
            "inv_bad_sum",
            "debtor",
            "accounting.Counterparty",
            "cp_customer",
        ),
        add_link(
            "accounting.Invoice",
            "inv_bad_sum",
            "book",
            "accounting.Book",
            "book_main",
        ),
    ]);
    assert!(
        cell.commit(mismatched_invoice).is_err(),
        "Invoice with amount != subtotal + tax_amount must be refused"
    );

    // 2. Seed valid draft invoices (one positive, one zero-total draft, one to void).
    let mut invoice_seed = CellCommit::empty();
    invoice_seed.writes.extend([
        put_fact(
            "accounting.Invoice",
            "inv_valid",
            &json!({
                "id": "inv_valid",
                "invoice_number": "INV-2026-0001",
                "direction": "receivable",
                "subtotal": 1000,
                "tax_amount": 150,
                "amount": 1150,
                "amount_paid": 0,
                "currency": "USD",
                "due_date": "2026-12-15",
                "status": "draft"
            }),
        ),
        put_fact(
            "accounting.Invoice",
            "inv_zero_draft",
            &json!({
                "id": "inv_zero_draft",
                "invoice_number": "INV-2026-0002",
                "direction": "receivable",
                "subtotal": 0,
                "tax_amount": 0,
                "amount": 0,
                "amount_paid": 0,
                "currency": "USD",
                "due_date": "2026-12-15",
                "status": "draft"
            }),
        ),
        put_fact(
            "accounting.Invoice",
            "inv_to_void",
            &json!({
                "id": "inv_to_void",
                "invoice_number": "INV-2026-0003",
                "direction": "receivable",
                "subtotal": 200,
                "tax_amount": 0,
                "amount": 200,
                "amount_paid": 0,
                "currency": "USD",
                "due_date": "2026-12-15",
                "status": "draft"
            }),
        ),
    ]);
    for invoice_id in ["inv_valid", "inv_zero_draft", "inv_to_void"] {
        invoice_seed.links.extend([
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
        ]);
    }
    cell.commit(invoice_seed).expect("draft invoices commit");

    // Issuing a zero-total draft is refused; issuing positive invoice succeeds.
    let zero_issue = fixture
        .invoke(
            &mut cell,
            "accounting.invoice.issue",
            &json!({"invoiceId": "inv_zero_draft"}),
            "principal:accountant",
            "inv-issue-zero-refused",
        )
        .expect_err("zero-amount draft invoice cannot be issued");
    assert_eq!(zero_issue.code(), "action.precondition_failed");

    fixture
        .invoke(
            &mut cell,
            "accounting.invoice.issue",
            &json!({"invoiceId": "inv_valid"}),
            "principal:accountant",
            "inv-issue-valid",
        )
        .expect("accountant issues positive draft invoice");
    assert_eq!(
        status_of(&cell, "accounting.Invoice", "inv_valid"),
        "issued"
    );

    let void_denied = fixture
        .invoke(
            &mut cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_to_void"}),
            "principal:accountant",
            "inv-void-accountant-denied",
        )
        .expect_err("accountant cannot void invoice");
    assert_eq!(void_denied.code(), "action.permission_denied");

    fixture
        .invoke(
            &mut cell,
            "accounting.invoice.void",
            &json!({"invoiceId": "inv_to_void"}),
            "principal:senior_accountant",
            "inv-void-senior-ok",
        )
        .expect("senior_accountant voids invoice");
    assert_eq!(
        status_of(&cell, "accounting.Invoice", "inv_to_void"),
        "voided"
    );

    // 3. Payment create-only immutability, positive-quantity guard, replay, and duplicate-ID refusal.
    let zero_payment = json!({
        "id": "pay_zero",
        "action": "transfer_ownership",
        "occurred_at": "2026-11-20T10:00:00Z",
        "quantity": "0",
        "unit": null,
        "currency": "USD",
        "payment_method": "bank_transfer",
        "reference_no": "WIRE-ZERO",
        "providerId": "cp_customer",
        "receiverId": "cp_company",
        "resourceId": "ma_operating_usd",
        "invoiceId": "inv_valid"
    });
    let zero_pay_refused = fixture
        .invoke(
            &mut cell,
            "accounting.payment.record",
            &zero_payment,
            "principal:accountant",
            "pay-record-zero-refused",
        )
        .expect_err("zero-quantity payment must be refused");
    assert_eq!(zero_pay_refused.code(), "action.precondition_failed");

    let valid_payment = json!({
        "id": "pay_001",
        "action": "transfer_ownership",
        "occurred_at": "2026-11-20T10:15:00Z",
        "quantity": "1150",
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
            &mut cell,
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

    // Exact retry with the same idempotency key replays cleanly.
    let replayed = fixture
        .invoke(
            &mut cell,
            "accounting.payment.record",
            &valid_payment,
            "principal:accountant",
            "pay-record-1",
        )
        .expect("exact retry replays");
    assert!(matches!(replayed, ActionOutcome::Replayed { .. }));

    // A distinct call targeting the same Payment ID is refused (create-only immutability).
    let duplicate_refused = fixture
        .invoke(
            &mut cell,
            "accounting.payment.record",
            &valid_payment,
            "principal:accountant",
            "pay-record-duplicate-id",
        )
        .expect_err("duplicate Payment ID must be refused by create_only write mode");
    assert_eq!(duplicate_refused.code(), "action.write_mode_failed");

    // Durable SQLite reopen preserves exact state and hash-chained ledger integrity.
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
