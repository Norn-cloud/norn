#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "inventory domain pack end-to-end kernel execution fixtures"
)]
//! End-to-end `norn-cell` execution suite for the Governed Inventory &
//! Warehouse Domain Pack (`registry/packs/inventory/`): static admission
//! against the Core REA + TimeInterval base, real SQLite cell definition
//! install with checker receipt, law and `core.TimeInterval` refusals,
//! create-only `StockMovement` immutability, `LotBatch` quarantine -> recall
//! terminal immutability, `StockReservation` and `CycleCount` lifecycles, and
//! fail-closed Cedar `AccessIR` role separation.

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
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite, LogicalTime, TypeCatalog, World, commit_action,
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

        // Grant every test principal broad action capabilities so that the
        // compiled Cedar AccessIR policy rules alone decide allow vs deny.
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
            InventoryPrincipals::new(&types, &interfaces, &properties, &actions, &policies);

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
            values: decode_json_object(values),
        },
    }
}

fn update_fact(object_type: &str, id: &str, expected: FactVersion, values: &Value) -> FactWrite {
    FactWrite {
        key: FactKey::new(object_type, id),
        expected,
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
        "inventory.StockBalance.non_negative_and_reserved_bounded.law",
        "inventory.StockReservation.positive_quantity.law",
        "inventory.StockMovement.positive_quantity.law",
        "inventory.Item.non_negative_reorder.law",
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

#[test]
fn inventory_laws_and_time_interval_enforce_in_real_cell() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-laws.sqlite");
    let mut cell = fixture.open_installed_cell(&path);

    let mut seed = CellCommit::empty();
    seed.writes.extend([
        put_fact(
            "inventory.Warehouse",
            "wh_central",
            &json!({
                "id": "wh_central",
                "warehouse_code": "WH-CENTRAL-01",
                "display_name": "Central Distribution Hub",
                "kind": "organization",
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.BinLocation",
            "bin_a01",
            &json!({
                "id": "bin_a01",
                "bin_code": "A-01-01",
                "zone": "AMBIENT",
                "max_capacity": 500,
                "unit": "{each}",
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.Item",
            "item_bearing",
            &json!({
                "id": "item_bearing",
                "sku": "SKU-BEARING-01",
                "gtin": "01234567890128",
                "display_name": "Precision Steel Bearing",
                "base_unit": "{each}",
                "valuation_method": "fifo",
                "track_lot": true,
                "reorder_point": 20,
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.StockBalance",
            "bal_central_bearing",
            &json!({
                "id": "bal_central_bearing",
                "resource_code": "SKU-BEARING-01@WH-CENTRAL-01",
                "quantity": 100,
                "reserved_quantity": 25,
                "unit": "{each}",
                "currency": null,
                "valuation_method": "fifo"
            }),
        ),
    ]);
    seed.links.extend([
        add_link(
            "inventory.BinLocation",
            "bin_a01",
            "warehouse",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_bearing",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_bearing",
            "item",
            "inventory.Item",
            "item_bearing",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_central_bearing",
            "bin",
            "inventory.BinLocation",
            "bin_a01",
        ),
    ]);
    cell.commit(seed)
        .expect("seed valid warehouse, bin, item, and stock balance");

    let snapshot_before_refusals = cell
        .store()
        .read()
        .dump()
        .expect("snapshot before law refusals");

    // 1. Refusal of negative StockBalance.quantity.
    let mut neg_balance = CellCommit::empty();
    neg_balance.writes.push(put_fact(
        "inventory.StockBalance",
        "bal_negative",
        &json!({
            "id": "bal_negative",
            "resource_code": "SKU-BEARING-01@WH-CENTRAL-01",
            "quantity": -1,
            "reserved_quantity": 0,
            "unit": "{each}",
            "currency": null,
            "valuation_method": "fifo"
        }),
    ));
    neg_balance.links.extend([
        add_link(
            "inventory.StockBalance",
            "bal_negative",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_negative",
            "item",
            "inventory.Item",
            "item_bearing",
        ),
    ]);
    assert!(
        cell.commit(neg_balance).is_err(),
        "negative StockBalance.quantity must be refused by inventory.StockBalance.non_negative_and_reserved_bounded.law"
    );

    // 2. Refusal of StockBalance with reserved_quantity > quantity.
    let mut over_reserved_balance = CellCommit::empty();
    over_reserved_balance.writes.push(put_fact(
        "inventory.StockBalance",
        "bal_over_reserved",
        &json!({
            "id": "bal_over_reserved",
            "resource_code": "SKU-BEARING-01@WH-CENTRAL-01",
            "quantity": 10,
            "reserved_quantity": 15,
            "unit": "{each}",
            "currency": null,
            "valuation_method": "fifo"
        }),
    ));
    over_reserved_balance.links.extend([
        add_link(
            "inventory.StockBalance",
            "bal_over_reserved",
            "owner",
            "inventory.Warehouse",
            "wh_central",
        ),
        add_link(
            "inventory.StockBalance",
            "bal_over_reserved",
            "item",
            "inventory.Item",
            "item_bearing",
        ),
    ]);
    assert!(
        cell.commit(over_reserved_balance).is_err(),
        "reserved_quantity > quantity must be refused by inventory.StockBalance.non_negative_and_reserved_bounded.law"
    );

    // 3. Refusal of Warehouse with kind != "organization".
    let mut person_warehouse = CellCommit::empty();
    person_warehouse.writes.push(put_fact(
        "inventory.Warehouse",
        "wh_person_invalid",
        &json!({
            "id": "wh_person_invalid",
            "warehouse_code": "WH-BAD-01",
            "display_name": "Invalid Person Warehouse",
            "kind": "person",
            "status": "active"
        }),
    ));
    assert!(
        cell.commit(person_warehouse).is_err(),
        "Warehouse with kind != organization must be refused by inventory.Warehouse.kind_is_organization.law"
    );

    // 4. Refusal of Item with negative reorder_point.
    let mut negative_reorder_item = CellCommit::empty();
    negative_reorder_item.writes.push(put_fact(
        "inventory.Item",
        "item_bad_reorder",
        &json!({
            "id": "item_bad_reorder",
            "sku": "SKU-BAD-01",
            "gtin": null,
            "display_name": "Invalid Reorder Item",
            "base_unit": "{each}",
            "valuation_method": "moving_average",
            "track_lot": false,
            "reorder_point": -5,
            "status": "active"
        }),
    ));
    assert!(
        cell.commit(negative_reorder_item).is_err(),
        "negative Item.reorder_point must be refused by inventory.Item.non_negative_reorder.law"
    );

    // 5. Refusal of CycleCount with bad variance_quantity.
    let mut bad_variance_count = CellCommit::empty();
    bad_variance_count.writes.push(put_fact(
        "inventory.CycleCount",
        "cc_bad_variance",
        &json!({
            "id": "cc_bad_variance",
            "starts_at": "2026-11-23T08:00:00Z",
            "ends_at": "2026-11-23T10:00:00Z",
            "timezone": "UTC",
            "expected_quantity": 50,
            "counted_quantity": 47,
            "variance_quantity": 2,
            "status": "draft"
        }),
    ));
    bad_variance_count.links.extend([
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
            "bal_central_bearing",
        ),
    ]);
    assert!(
        cell.commit(bad_variance_count).is_err(),
        "CycleCount with mismatched variance_quantity must be refused by inventory.CycleCount.variance_conservation.law"
    );

    // 6. Refusal of CycleCount with degenerate or reversed TimeInterval (starts_at >= ends_at).
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
        let mut bad_interval_count = CellCommit::empty();
        bad_interval_count.writes.push(put_fact(
            "inventory.CycleCount",
            count_id,
            &json!({
                "id": count_id,
                "starts_at": starts_at,
                "ends_at": ends_at,
                "timezone": "UTC",
                "expected_quantity": 50,
                "counted_quantity": 50,
                "variance_quantity": 0,
                "status": "draft"
            }),
        ));
        bad_interval_count.links.extend([
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
                "bal_central_bearing",
            ),
        ]);
        assert!(
            cell.commit(bad_interval_count).is_err(),
            "CycleCount {count_id} with starts_at >= ends_at must be refused by inherited core.TimeInterval proper_interval invariant"
        );
    }

    // 7. Refusal of StockReservation with zero quantity.
    let mut zero_reservation = CellCommit::empty();
    zero_reservation.writes.push(put_fact(
        "inventory.StockReservation",
        "res_zero",
        &json!({
            "id": "res_zero",
            "status": "open",
            "quantity": 0,
            "unit": "{each}",
            "currency": null,
            "expires_at": "2026-11-26T00:00:00Z"
        }),
    ));
    zero_reservation.links.extend([
        add_link(
            "inventory.StockReservation",
            "res_zero",
            "resource",
            "inventory.StockBalance",
            "bal_central_bearing",
        ),
        add_link(
            "inventory.StockReservation",
            "res_zero",
            "holder",
            "inventory.Warehouse",
            "wh_central",
        ),
    ]);
    assert!(
        cell.commit(zero_reservation).is_err(),
        "zero-quantity StockReservation must be refused by inventory.StockReservation.positive_quantity.law"
    );

    // None of the refused writes may have mutated cell state.
    assert_eq!(
        cell.store()
            .read()
            .dump()
            .expect("snapshot after law refusals"),
        snapshot_before_refusals
    );
}

#[test]
fn inventory_actions_immutability_and_cedar_access_enforce_in_real_cell() {
    let fixture = InventoryFixture::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("inventory-actions.sqlite");
    let mut cell = fixture.open_installed_cell(&path);

    let mut seed = CellCommit::empty();
    seed.writes.extend([
        put_fact(
            "inventory.Warehouse",
            "wh_central",
            &json!({
                "id": "wh_central",
                "warehouse_code": "WH-CENTRAL-01",
                "display_name": "Central Distribution Hub",
                "kind": "organization",
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.Warehouse",
            "wh_regional",
            &json!({
                "id": "wh_regional",
                "warehouse_code": "WH-REGIONAL-02",
                "display_name": "Regional Fulfillment Annex",
                "kind": "organization",
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.Item",
            "item_valve",
            &json!({
                "id": "item_valve",
                "sku": "SKU-VALVE-09",
                "gtin": "00012345678905",
                "display_name": "Hydraulic Check Valve",
                "base_unit": "{each}",
                "valuation_method": "fifo",
                "track_lot": true,
                "reorder_point": 15,
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.LotBatch",
            "lot_2026_a",
            &json!({
                "id": "lot_2026_a",
                "lot_code": "LOT-2026-11-A",
                "manufactured_on": "2026-11-01",
                "expires_on": "2028-11-01",
                "status": "active"
            }),
        ),
        put_fact(
            "inventory.StockBalance",
            "bal_central_valve",
            &json!({
                "id": "bal_central_valve",
                "resource_code": "SKU-VALVE-09@WH-CENTRAL-01",
                "quantity": 120,
                "reserved_quantity": 30,
                "unit": "{each}",
                "currency": null,
                "valuation_method": "fifo"
            }),
        ),
        put_fact(
            "inventory.StockReservation",
            "res_consume_target",
            &json!({
                "id": "res_consume_target",
                "status": "open",
                "quantity": 10,
                "unit": "{each}",
                "currency": null,
                "expires_at": "2026-11-26T00:00:00Z"
            }),
        ),
        put_fact(
            "inventory.StockReservation",
            "res_release_target",
            &json!({
                "id": "res_release_target",
                "status": "open",
                "quantity": 5,
                "unit": "{each}",
                "currency": null,
                "expires_at": "2026-11-26T00:00:00Z"
            }),
        ),
        put_fact(
            "inventory.CycleCount",
            "cc_nov_2026",
            &json!({
                "id": "cc_nov_2026",
                "starts_at": "2026-11-23T08:00:00Z",
                "ends_at": "2026-11-23T11:30:00Z",
                "timezone": "UTC",
                "expected_quantity": 120,
                "counted_quantity": 118,
                "variance_quantity": -2,
                "status": "draft"
            }),
        ),
    ]);
    seed.links.extend([
        add_link(
            "inventory.Warehouse",
            "wh_regional",
            "parent",
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
            "lot",
            "inventory.LotBatch",
            "lot_2026_a",
        ),
        add_link(
            "inventory.StockReservation",
            "res_consume_target",
            "resource",
            "inventory.StockBalance",
            "bal_central_valve",
        ),
        add_link(
            "inventory.StockReservation",
            "res_consume_target",
            "holder",
            "inventory.Warehouse",
            "wh_regional",
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
    ]);
    cell.commit(seed)
        .expect("seed warehouses, item, lot, stock balance, reservations, and cycle count");

    // 1. StockMovement create-only immutability: first create succeeds with links,
    //    exact retry replays, and a distinct call reusing the same ID is refused.
    let movement_input = json!({
        "id": "mov_transfer_001",
        "action": "transfer_custody",
        "movement_kind": "transfer",
        "occurred_at": "2026-11-23T12:00:00Z",
        "quantity": "25",
        "unit": "{each}",
        "unit_cost": "14.50",
        "currency": "USD",
        "reference_code": "TRF-2026-0001",
        "providerId": "wh_central",
        "receiverId": "wh_regional",
        "resourceId": "bal_central_valve"
    });
    fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &movement_input,
            "principal:warehouse_clerk",
            "mov-record-1",
        )
        .expect("warehouse clerk records create-only stock movement");
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new("inventory.StockMovement", "mov_transfer_001"),
            "resource",
            FactKey::new("inventory.StockBalance", "bal_central_valve"),
        ))
        .expect("resource link loads")
        .exists()
    );
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new("inventory.StockMovement", "mov_transfer_001"),
            "receiver",
            FactKey::new("inventory.Warehouse", "wh_regional"),
        ))
        .expect("receiver link loads")
        .exists()
    );

    let replayed = fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &movement_input,
            "principal:warehouse_clerk",
            "mov-record-1",
        )
        .expect("exact stock movement retry replays");
    assert!(matches!(replayed, ActionOutcome::Replayed { .. }));

    let duplicate_movement = fixture
        .invoke(
            &mut cell,
            "inventory.movement.record",
            &movement_input,
            "principal:warehouse_clerk",
            "mov-record-overwrite-attempt",
        )
        .expect_err("duplicate StockMovement ID must be refused by create_only writeMode");
    assert_eq!(duplicate_movement.code(), "action.write_mode_failed");

    // 2. StockReservation lifecycle transitions (consume and release).
    fixture
        .invoke(
            &mut cell,
            "inventory.reservation.consume",
            &json!({"reservationId": "res_consume_target"}),
            "principal:warehouse_clerk",
            "res-consume-1",
        )
        .expect("warehouse clerk consumes open stock reservation");
    assert_eq!(
        status_of(&cell, "inventory.StockReservation", "res_consume_target"),
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
        .expect("warehouse clerk releases open stock reservation");
    assert_eq!(
        status_of(&cell, "inventory.StockReservation", "res_release_target"),
        "released"
    );

    // 3. LotBatch quarantine -> recall + fail-closed Cedar denial for warehouse_clerk
    //    + terminal immutability once recalled.
    fixture
        .invoke(
            &mut cell,
            "inventory.lot.quarantine",
            &json!({"lotId": "lot_2026_a"}),
            "principal:warehouse_clerk",
            "lot-quarantine-1",
        )
        .expect("warehouse clerk may place active lot into quarantine");
    assert_eq!(
        status_of(&cell, "inventory.LotBatch", "lot_2026_a"),
        "quarantined"
    );

    let clerk_recall = fixture
        .invoke(
            &mut cell,
            "inventory.lot.recall",
            &json!({"lotId": "lot_2026_a"}),
            "principal:warehouse_clerk",
            "lot-recall-clerk-denied",
        )
        .expect_err("warehouse clerk must be denied inventory.lot.recall by Cedar policy");
    assert_eq!(clerk_recall.code(), "action.permission_denied");
    assert_eq!(
        status_of(&cell, "inventory.LotBatch", "lot_2026_a"),
        "quarantined"
    );

    fixture
        .invoke(
            &mut cell,
            "inventory.lot.recall",
            &json!({"lotId": "lot_2026_a"}),
            "principal:quality_inspector",
            "lot-recall-inspector-1",
        )
        .expect("quality inspector permanently recalls quarantined lot");
    assert_eq!(
        status_of(&cell, "inventory.LotBatch", "lot_2026_a"),
        "recalled"
    );

    let recalled_state = cell
        .load(&FactKey::new("inventory.LotBatch", "lot_2026_a"))
        .expect("recalled lot loads");
    let mut mutate_recalled = CellCommit::empty();
    mutate_recalled.writes.push(update_fact(
        "inventory.LotBatch",
        "lot_2026_a",
        recalled_state.version,
        &json!({
            "id": "lot_2026_a",
            "lot_code": "LOT-2026-11-A-TAMPERED",
            "manufactured_on": "2026-11-01",
            "expires_on": "2028-11-01",
            "status": "recalled"
        }),
    ));
    assert!(
        cell.commit(mutate_recalled).is_err(),
        "recalled LotBatch must enforce terminal immutability"
    );

    // 4. CycleCount submit -> approve -> post with fail-closed Cedar checks
    //    and terminal immutability once posted.
    fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.submit",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:warehouse_clerk",
            "cc-submit-1",
        )
        .expect("warehouse clerk submits draft cycle count");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "submitted"
    );

    let clerk_approve = fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.approve",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:warehouse_clerk",
            "cc-approve-clerk-denied",
        )
        .expect_err("warehouse clerk must be denied inventory.cycle_count.approve by Cedar policy");
    assert_eq!(clerk_approve.code(), "action.permission_denied");

    let inspector_approve = fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.approve",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:quality_inspector",
            "cc-approve-inspector-denied",
        )
        .expect_err(
            "quality inspector must be denied inventory.cycle_count.approve by Cedar policy",
        );
    assert_eq!(inspector_approve.code(), "action.permission_denied");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "submitted"
    );

    fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.approve",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:inventory_manager",
            "cc-approve-manager-1",
        )
        .expect("inventory manager approves submitted cycle count");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "approved"
    );

    let clerk_post = fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.post",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:warehouse_clerk",
            "cc-post-clerk-denied",
        )
        .expect_err("warehouse clerk must be denied inventory.cycle_count.post by Cedar policy");
    assert_eq!(clerk_post.code(), "action.permission_denied");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "approved"
    );

    fixture
        .invoke(
            &mut cell,
            "inventory.cycle_count.post",
            &json!({"cycleCountId": "cc_nov_2026"}),
            "principal:inventory_manager",
            "cc-post-manager-1",
        )
        .expect("inventory manager posts approved cycle count");
    assert_eq!(
        status_of(&cell, "inventory.CycleCount", "cc_nov_2026"),
        "posted"
    );

    let posted_count_state = cell
        .load(&FactKey::new("inventory.CycleCount", "cc_nov_2026"))
        .expect("posted cycle count loads");
    let mut mutate_posted_count = CellCommit::empty();
    mutate_posted_count.writes.push(update_fact(
        "inventory.CycleCount",
        "cc_nov_2026",
        posted_count_state.version,
        &json!({
            "id": "cc_nov_2026",
            "starts_at": "2026-11-23T08:00:00Z",
            "ends_at": "2026-11-23T11:30:00Z",
            "timezone": "UTC",
            "expected_quantity": 120,
            "counted_quantity": 120,
            "variance_quantity": 0,
            "status": "posted"
        }),
    ));
    assert!(
        cell.commit(mutate_posted_count).is_err(),
        "posted CycleCount must enforce terminal immutability"
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
