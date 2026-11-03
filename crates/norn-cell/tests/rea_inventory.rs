#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "declared CP3 correspondence oracle"
)]
//! INV-CP1: declared receipt/delivery, FIFO and moving average use native SQLite.
//! Runtime correspondence only; no proof or external payment outcome is claimed.

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::{ActionRoot, InterfaceDef, ObjectTypeDef, PropertyTypeDef, RuleRoot};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, CellCommit,
    CellHost, Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LogicalTime, SynchronousRule, TypeCatalog, World, commit_action,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

#[path = "rea_inventory/parity.rs"]
mod parity;

type NativeCell = SqliteCell<FactEvent, JsonObject, JsonObject>;
struct Cell {
    host: NativeCell,
    path: PathBuf,
}
impl Deref for Cell {
    type Target = NativeCell;
    fn deref(&self) -> &Self::Target {
        &self.host
    }
}
impl DerefMut for Cell {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.host
    }
}
type Refusal = Box<norn_kernel::ActionRefusal<norn_cell::CellError>>;
const PREFIX: &str = "inventory.Cycle";
const KNOWN_AT: u64 = 1_793_664_000_000;

struct Fixture {
    types: Vec<ObjectTypeDef>,
    properties: Vec<PropertyTypeDef>,
    interfaces: Vec<InterfaceDef>,
    actions: BTreeMap<String, ActionRoot>,
    rules: Vec<SynchronousRule>,
    laws: Vec<norn_ir::ArtifactEnvelope>,
    principals: Principals,
    admission_hash: String,
    environment_hash: String,
    expr_profile: String,
}
impl Fixture {
    fn load() -> Self {
        let mut documents: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/rea-inventory.json")).unwrap();
        let helpers: Vec<Value> = serde_json::from_str(include_str!(
            "../../norn-decisions/tests/fixtures/jewelry-folds.json"
        ))
        .unwrap();
        let required_helpers = ["jewelry.fold.allocate_step.rule"];
        let helpers: Vec<_> = helpers
            .into_iter()
            .filter(|document| {
                required_helpers
                    .iter()
                    .any(|id| document["artifactId"].as_str() == Some(id))
            })
            .collect();
        assert_eq!(
            helpers.len(),
            required_helpers.len(),
            "the declared A5 allocation helper is retained"
        );
        documents.extend(helpers);
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/core/artifacts");
        for name in [
            "core.Agent.interface",
            "core.Resource.interface",
            "core.Commitment.interface",
            "core.Hold.interface",
            "core.EconomicEvent.interface",
            "core.LedgerEffect.interface",
            "core.Claim.interface",
            "core.Process.interface",
            "core.Exchange.interface",
            "core.value_types.entity",
            "std.value_types.entity",
            "core.units",
        ] {
            documents.push(
                serde_json::from_str(
                    &std::fs::read_to_string(directory.join(format!("{name}.json"))).unwrap(),
                )
                .unwrap(),
            );
        }
        let artifacts: Vec<norn_ir::ArtifactEnvelope> =
            serde_json::from_value(Value::Array(documents.clone())).unwrap();
        let registry = std::collections::BTreeSet::new();
        let sources: Vec<_> = documents
            .iter()
            .cloned()
            .map(norn_ir::RuntimeValue::from_json)
            .collect::<Result<_, _>>()
            .unwrap();
        let sources = norn_checker::receipt::ReceiptSources::new(&sources, &[]).unwrap();
        let checked = norn_checker::receipt::check_artifact_set_with_receipt(
            &sources,
            &norn_checker::EntityArtifactSetContext {
                registry: &registry,
                base: &[],
                tenant: None,
            },
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        )
        .unwrap();
        eprintln!(
            "INV-CP1 definitions={} profile={} environment={} scope=org_demo/dev world=main effective_inventory=2026-11-03T00:00:00Z knowledge=2026-11-03T00:00:00Z frontier=local-commit-order fold_semantics=1.0 arithmetic=scale6/refuse",
            checked.receipt.artifact_set.sha256,
            checked.receipt.expr_profile,
            checked
                .receipt
                .type_env
                .as_ref()
                .map_or("missing", |e| e.sha256.as_str())
        );
        assert!(checked.receipt.admitted, "authored app admission: {}", serde_json::to_string(&checked.diagnostics.iter().map(|d| json!({"code": d.code.as_str(), "message": d.message, "artifact": d.artifact_id})).collect::<Vec<_>>()).unwrap());
        let mut types = Vec::new();
        let mut properties = Vec::new();
        let mut interfaces = Vec::new();
        let mut actions = BTreeMap::new();
        let mut roots = BTreeMap::<String, RuleRoot>::new();
        for document in &documents {
            let payload = &document["payload"];
            if let Some(definition) = payload.get("objectType") {
                types.push(serde_json::from_value(definition.clone()).unwrap());
            }
            if let Some(definition) = payload.get("interface") {
                interfaces.push(serde_json::from_value(definition.clone()).unwrap());
            }
            if let Some(Value::Array(definitions)) = payload.get("propertyTypes") {
                for definition in definitions {
                    properties.push(serde_json::from_value(definition.clone()).unwrap());
                }
            }
            if document["kind"] == "action" {
                let action: ActionRoot = serde_json::from_value(payload.clone()).unwrap();
                actions.insert(action.action_id.clone(), action);
            }
            if document["kind"] == "rule" {
                roots.insert(
                    document["artifactId"].as_str().unwrap().into(),
                    serde_json::from_value(payload.clone()).unwrap(),
                );
            }
        }
        types.push(action_call_type());
        let rules = documents
            .iter()
            .filter(|document| {
                document["kind"] == "rule" && document["payload"]["contract"] == "decide"
            })
            .map(|document| {
                let id = document["artifactId"].as_str().unwrap();
                SynchronousRule::admit(
                    norn_ir::ActionRuleRef {
                        artifact_id: id.into(),
                        version: document["version"].as_str().unwrap().into(),
                    },
                    &roots[id],
                    &writer_helpers(id, &artifacts, &roots),
                    norn_kernel::DecisionBinding {
                        profile_id: checked.receipt.expr_profile.clone(),
                        environment_hash: checked
                            .receipt
                            .type_env
                            .as_ref()
                            .expect("admitted type environment")
                            .sha256
                            .clone(),
                    },
                )
                .unwrap()
            })
            .collect();
        let access: norn_ir::access::AccessIr = serde_json::from_value(
            documents.iter().find(|d| d["kind"] == "policy").unwrap()["payload"].clone(),
        )
        .unwrap();
        let principals = Principals::new(&types, &interfaces, &properties, &actions, &access);
        let laws = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == norn_ir::ArtifactKind::Law)
            .cloned()
            .collect();
        Self {
            types,
            properties,
            interfaces,
            actions,
            rules,
            laws,
            principals,
            admission_hash: checked.receipt.artifact_set.sha256,
            environment_hash: checked.receipt.type_env.unwrap().sha256,
            expr_profile: checked.receipt.expr_profile,
        }
    }
    fn model(&self) -> Model {
        Model::from_definitions_with_interfaces(&self.types, &self.properties, &self.interfaces)
            .unwrap()
            .with_governing_laws(&self.laws)
            .unwrap()
    }
    fn catalog(&self) -> TypeCatalog<'_> {
        TypeCatalog::new(&self.types)
            .with_property_types(&self.properties)
            .with_interfaces(&self.interfaces)
            .with_action_rules(&self.rules)
            .with_governing_laws(&self.laws)
    }
    fn invoke(
        &self,
        cell: &mut Cell,
        name: &str,
        input: Value,
        actor: &str,
        operation: &str,
    ) -> Result<ActionOutcome, Refusal> {
        let outcome = self.invoke_host(&mut cell.host, name, input, actor, operation);
        if outcome.is_ok() {
            let reopened = open(&cell.path, self);
            assert_eq!(
                reopened.store().read().dump().unwrap(),
                cell.store().read().dump().unwrap(),
                "{name}: complete durable state after commit/replay"
            );
            assert_eq!(
                reopened.store().read().verify_ledger().unwrap(),
                cell.store().read().verify_ledger().unwrap()
            );
            *cell = reopened;
        }
        outcome
    }
    fn invoke_host(
        &self,
        host: &mut impl CellHost<
            Event = FactEvent,
            Command = JsonObject,
            Input = JsonObject,
            Error = norn_cell::CellError,
        >,
        name: &str,
        input: Value,
        actor: &str,
        operation: &str,
    ) -> Result<ActionOutcome, Refusal> {
        self.invoke_scoped(
            host,
            name,
            input,
            actor,
            operation,
            BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]),
        )
    }
    fn invoke_scoped(
        &self,
        host: &mut impl CellHost<
            Event = FactEvent,
            Command = JsonObject,
            Input = JsonObject,
            Error = norn_cell::CellError,
        >,
        name: &str,
        input: Value,
        actor: &str,
        operation: &str,
        dimensions: BTreeMap<String, String>,
    ) -> Result<ActionOutcome, Refusal> {
        let action_id = if name.contains('.') {
            name.to_owned()
        } else {
            format!("inventory.cycle.{name}")
        };
        let action = &self.actions[&action_id];
        let call = ActionCall {
            action,
            action_artifact_id: &format!("{action_id}.action"),
            action_version: "1.0.0",
            object_type: None,
            input: serde_json::from_value(input).unwrap(),
            actor: actor.into(),
            scope: dimensions,
            idempotency_key: operation.into(),
            turn: DecisionTurn {
                id: format!("turn:{operation}"),
                world: World::Main,
                now: LogicalTime::from_millis(KNOWN_AT),
                consumes: None,
            },
        };
        commit_action(host, &self.catalog(), &call, &self.principals).map_err(Box::new)
    }
}

fn writer_helpers(
    id: &str,
    artifacts: &[norn_ir::ArtifactEnvelope],
    roots: &BTreeMap<String, RuleRoot>,
) -> BTreeMap<String, RuleRoot> {
    let mut helpers = BTreeMap::new();
    let mut pending = vec![id.to_owned()];
    while let Some(current) = pending.pop() {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == current)
            .expect("checked rule dependency");
        for pin in artifact
            .dependencies
            .iter()
            .filter(|pin| pin.kind == norn_ir::ArtifactKind::Rule)
        {
            let target = artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == pin.artifact_id)
                .expect("retained helper");
            assert_eq!(pin.version.as_deref(), Some(target.version.as_str()));
            if !helpers.contains_key(&pin.artifact_id) {
                helpers.insert(pin.artifact_id.clone(), roots[&pin.artifact_id].clone());
                pending.push(pin.artifact_id.clone());
            }
        }
    }
    helpers
}

// The fixture supplies scoped principal/grant inputs to the real Cedar PDP.
struct Principals {
    pdp: Pdp,
}
fn scope() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        tenant: None,
        workspace: None,
        environment: Some("dev".into()),
        business_unit: None,
    }
}
fn roles(actor: &str) -> Vec<String> {
    if actor == "principal:operator" {
        vec!["Operator".into()]
    } else {
        vec![]
    }
}
impl Principals {
    fn new(
        types: &[ObjectTypeDef],
        interfaces: &[InterfaceDef],
        properties: &[PropertyTypeDef],
        actions: &BTreeMap<String, ActionRoot>,
        access: &norn_ir::access::AccessIr,
    ) -> Self {
        let actions: Vec<_> = actions.values().cloned().collect();
        let projection = norn_projections::project_cedar_schema_with_catalog(
            types, interfaces, properties, &actions,
        );
        let host = norn_projections::project_host_access_schema(types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .unwrap();
        let grants = ["principal:operator"]
            .into_iter()
            .map(|actor| {
                let mut capabilities: Vec<String> =
                    actions.iter().map(|a| a.action_id.clone()).collect();
                capabilities.push("norn.read".into());
                CapabilityGrant {
                    subject_id: actor.into(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    scope: Some(scope()),
                    capabilities,
                    binding: None,
                }
            })
            .collect::<Vec<_>>();
        Self {
            pdp: Pdp::new(authz::compile(&schema, access).unwrap(), &grants).unwrap(),
        }
    }
    fn request(
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
        let request = AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: roles(actor),
                attributes: BTreeMap::new(),
            },
            scope: Some(request_scope.clone()),
            capability: action.into(),
            action_id: action.into(),
            resource: ResourceInput {
                object_type: target.object_type.as_str().into(),
                id: target.key.clone(),
                scope: request_scope,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        };
        Ok(request)
    }
    fn check(
        &self,
        actor: &str,
        action: &str,
        target: &FactKey,
        dimensions: &BTreeMap<String, String>,
    ) -> Result<(), ActionDenial> {
        let request = Self::request(actor, action, target, dimensions)?;
        match self.pdp.decide(&request) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(reason) => Err(ActionDenial::Forbidden {
                reason: reason.code().into(),
            }),
        }
    }
}
impl ActionAuthorizer for Principals {
    fn authorize(&self, r: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        PdpActionAuthorizer::new(self.pdp.clone(), |r: &ActionAuthzRequest<'_>| {
            Self::request(r.actor, &r.action.action_id, r.target, r.scope)
        })
        .authorize(r)
    }
    fn authorize_read(
        &self,
        r: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.check(r.actor, "norn.read", r.target, r.scope)
    }
    fn authorize_read_type(
        &self,
        r: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.check(
            r.actor,
            "norn.read",
            &FactKey::new(r.object_type.as_str(), "collection"),
            r.scope,
        )
    }
}
fn action_call_type() -> ObjectTypeDef {
    let required = ["actionId", "actor", "turn", "transition", "from", "to"];
    let text = [
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "ruleBinding",
    ];
    let mut fields: Vec<Value> = required
        .iter()
        .map(|id| json!({"id": id, "type": {"kind":"text"}}))
        .collect();
    fields.extend(
        text.iter()
            .map(|id| json!({"id":id,"type":{"kind":"text"},"constraints":{"nullable":true}})),
    );
    fields.extend(
        [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ]
        .iter()
        .map(|id| json!({"id":id,"type":{"kind":"json-ext"},"constraints":{"nullable":true}})),
    );
    serde_json::from_value(json!({"name":"norn.ActionCall","properties":fields})).unwrap()
}
fn open(path: &Path, fixture: &Fixture) -> Cell {
    let mut host = NativeCell::open(
        path,
        fixture.model(),
        StoreOptions::default(),
        "org_demo/dev",
        |_: &Command<JsonObject>| panic!("payment is enqueue-only"),
    )
    .unwrap();
    host.advance_to(LogicalTime::from_millis(KNOWN_AT));
    Cell {
        host,
        path: path.into(),
    }
}
fn key(kind: &str, id: &str) -> FactKey {
    FactKey::new(format!("{PREFIX}{kind}"), id)
}
fn values(cell: &Cell, kind: &str, id: &str) -> JsonObject {
    cell.load(&key(kind, id)).unwrap().values.unwrap()
}
fn exact(cell: &Cell, kind: &str, id: &str, field: &str, number: &str) {
    assert_eq!(
        values(cell, kind, id)[field],
        JsonValue::Number(number.parse().unwrap()),
        "{kind}/{id}.{field}"
    );
}
fn state(cell: &Cell, kind: &str, id: &str, expected: &str) {
    assert_eq!(values(cell, kind, id)["status"].as_str(), Some(expected));
}
fn refuses(
    fixture: &Fixture,
    cell: &mut Cell,
    name: &str,
    input: Value,
    actor: &str,
    operation: &str,
) -> norn_kernel::ActionRefusal<norn_cell::CellError> {
    let before = cell.store().read().dump().unwrap();
    let head = cell.store().read().verify_ledger().unwrap();
    let error = fixture
        .invoke(cell, name, input, actor, operation)
        .unwrap_err();
    eprintln!("{operation}: {error:?}");
    if operation.contains("overdraw") {
        assert_eq!(error.code(), "decision.guard_false");
    }
    assert_eq!(cell.store().read().dump().unwrap(), before);
    assert_eq!(cell.store().read().verify_ledger().unwrap(), head);
    let reopened = open(&cell.path, fixture);
    assert_eq!(reopened.store().read().dump().unwrap(), before);
    assert_eq!(reopened.store().read().verify_ledger().unwrap(), head);
    *error
}
fn seed_book(cell: &mut Cell, opening: bool) {
    assert!(!opening, "inventory tests start from an empty book");
    let mut proposal = CellCommit::empty();
    for (kind, id, value) in [
        (
            "Agent",
            "BUSINESS",
            json!({"id":"BUSINESS","display_name":"Synthetic business","kind":"organization"}),
        ),
        (
            "Agent",
            "PARTY",
            json!({"id":"PARTY","display_name":"Synthetic counterparty","kind":"person"}),
        ),
        ("Vault", "VAULT", json!({"id":"VAULT","company":"BUSINESS"})),
    ] {
        proposal.writes.push(FactWrite {
            key: key(kind, id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(value).unwrap(),
            },
        });
    }
    cell.commit(proposal).unwrap();
}
fn scaled(value: &JsonValue, places: usize) -> i128 {
    let number = value.as_decimal().unwrap().canonical();
    let fraction = number.split_once('.').map_or(0, |(_, f)| f.len());
    assert!(fraction <= places, "{number} exceeds declared scale");
    number
        .replace('.', "")
        .parse::<i128>()
        .unwrap()
        .checked_mul(10_i128.pow(u32::try_from(places - fraction).unwrap()))
        .unwrap()
}
fn rows(cell: &Cell, kind: &str) -> Vec<JsonObject> {
    let (rows, complete) = cell
        .store()
        .read()
        .root_rows("main", &format!("{PREFIX}{kind}"), 256)
        .unwrap();
    assert!(complete, "the state assertion must enumerate the full type");
    rows.into_iter()
        .map(|(id, _)| values(cell, kind, &id))
        .collect()
}
fn checkpoint(cell: &Cell, scenario: &str, objects: &[(&str, &str)]) {
    let state: BTreeMap<_, _> = objects
        .iter()
        .map(|(kind, id)| (format!("{kind}/{id}"), values(cell, kind, id)))
        .collect();
    let dump = serde_json::to_vec(&cell.store().read().dump().unwrap()).unwrap();
    eprintln!(
        "T1-C1 {scenario} dump_sha256={:x} objects={}",
        Sha256::digest(dump),
        serde_json::to_string(&state).unwrap()
    );
}
fn accounts(cell: &Cell, expected: &[(&str, &str)]) {
    let mut actual = BTreeMap::<String, i128>::new();
    for row in rows(cell, "Leg") {
        assert_eq!(row["currency"].as_str(), Some("USD"));
        *actual
            .entry(row["account_code"].as_str().unwrap().into())
            .or_default() += scaled(&row["amount"], 2);
    }
    assert_eq!(
        actual.values().sum::<i128>(),
        0,
        "every currency posting balances"
    );
    let expected: BTreeMap<_, _> = expected
        .iter()
        .map(|(account, amount)| {
            (
                String::from(*account),
                scaled(&JsonValue::Number(amount.parse().unwrap()), 2),
            )
        })
        .collect();
    assert_eq!(actual, expected, "complete USD account balances");
}
fn inventory(cell: &Cell, quantity: &str, value: &str, loss: &str, gross: &str) {
    let mut available = 0;
    let mut book_value = 0;
    let mut discarded = 0;
    let mut gross_total = 0;
    for row in rows(cell, "Holding") {
        assert_eq!(row["unit"].as_str(), Some("g"));
        assert_eq!(row["valuation_currency"].as_str(), Some("USD"));
        assert!(row["currency"].is_null());
        gross_total += scaled(&row["gross"], 3);
        match row["disposition"].as_str().unwrap() {
            "available" => {
                available += scaled(&row["quantity"], 3);
                book_value += scaled(&row["value"], 2);
            }
            "lost" => {
                discarded += scaled(&row["quantity"], 3);
            }
            "byproduct" => assert_eq!(scaled(&row["quantity"], 3), 0),
            disposition => panic!("unknown disposition {disposition}"),
        }
    }
    for (actual, expected, scale) in [
        (available, quantity, 3),
        (book_value, value, 2),
        (discarded, loss, 3),
        (gross_total, gross, 3),
    ] {
        assert_eq!(
            actual,
            scaled(&JsonValue::Number(expected.parse().unwrap()), scale)
        );
    }
}
fn receive(id: &str, quantity: i32, rate: i32, method: &str) -> Value {
    json!({"id":format!("RECEIPT-{id}"),"holding":id,"vault":"VAULT","company":"BUSINESS","party":"PARTY","event":format!("{id}-EVENT"),"debit":format!("{id}-INVENTORY"),"credit":format!("{id}-RNB"),"quantity":quantity,"rate":rate,"valuation_method":method})
}
fn deliver() -> Value {
    json!({"allocation1":"DELIVERY-ALLOCATION-1","allocation2":"DELIVERY-ALLOCATION-2","id":"DELIVERY","first":"R1","second":"R2","company":"BUSINESS","party":"PARTY","event":"DELIVERY-EVENT","debit":"DELIVERY-COGS","credit":"DELIVERY-STOCK","quantity":4})
}
fn cp1(
    method: &str,
    cost: &str,
    first_quantity: &str,
    first_value: &str,
    second_quantity: &str,
    second_value: &str,
) {
    let fixture = Fixture::load();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inventory.sqlite");
    let mut cell = open(&path, &fixture);
    seed_book(&mut cell, false);
    for (id, quantity, rate) in [("R1", 2, 10), ("R2", 3, 20)] {
        fixture
            .invoke(
                &mut cell,
                "createexchange",
                json!({"id":format!("RECEIPT-{id}")}),
                "principal:operator",
                &format!("draft-{id}"),
            )
            .unwrap();
        fixture
            .invoke(
                &mut cell,
                "receive",
                receive(id, quantity, rate, method),
                "principal:operator",
                &format!("receive-{id}"),
            )
            .unwrap();
        exact(&cell, "Holding", id, "quantity", &quantity.to_string());
    }
    exact(&cell, "Leg", "R1-INVENTORY", "amount", "20");
    exact(&cell, "Leg", "R1-RNB", "amount", "-20");
    exact(&cell, "Leg", "R2-INVENTORY", "amount", "60");
    exact(&cell, "Leg", "R2-RNB", "amount", "-60");
    fixture
        .invoke(
            &mut cell,
            "createexchange",
            json!({"id":"DELIVERY"}),
            "principal:operator",
            "draft-delivery",
        )
        .unwrap();
    let mut excessive = deliver();
    excessive["quantity"] = json!(6);
    refuses(
        &fixture,
        &mut cell,
        "deliver",
        excessive,
        "principal:operator",
        "overdraw",
    );
    fixture
        .invoke(
            &mut cell,
            "deliver",
            deliver(),
            "principal:operator",
            "deliver",
        )
        .unwrap();
    state(&cell, "Exchange", "DELIVERY", "posted");
    exact(&cell, "Leg", "DELIVERY-COGS", "amount", cost);
    exact(
        &cell,
        "Leg",
        "DELIVERY-STOCK",
        "amount",
        &format!("-{cost}"),
    );
    exact(&cell, "Holding", "R1", "quantity", first_quantity);
    exact(&cell, "Holding", "R1", "value", first_value);
    exact(&cell, "Holding", "R2", "quantity", second_quantity);
    exact(&cell, "Holding", "R2", "value", second_value);
    assert!(
        cell.load_link(&LinkKey::new(
            key("Allocation", "DELIVERY-ALLOCATION-1"),
            "source",
            key("Holding", "R1")
        ))
        .unwrap()
        .exists()
    );
    assert!(
        cell.load_link(&LinkKey::new(
            key("Allocation", "DELIVERY-ALLOCATION-2"),
            "source",
            key("Holding", "R2")
        ))
        .unwrap()
        .exists()
    );
    checkpoint(
        &cell,
        &format!("INV-CP1/{method}"),
        &[
            ("Holding", "R1"),
            ("Holding", "R2"),
            ("Leg", "DELIVERY-COGS"),
        ],
    );
    accounts(
        &cell,
        &[
            ("Inventory", if method == "fifo" { "20" } else { "16" }),
            ("ReceivedNotBilled", "-80"),
            ("COGS", cost),
        ],
    );
    inventory(
        &cell,
        "1",
        if method == "fifo" { "20" } else { "16" },
        "0",
        "1",
    );
    assert!(cell.store().read().pending_commands().unwrap().is_empty());
    let before = cell.store().read().dump().unwrap();
    let head = cell.store().read().verify_ledger().unwrap();
    drop(cell);
    let mut cell = open(&path, &fixture);
    assert_eq!(cell.store().read().dump().unwrap(), before);
    assert_eq!(cell.store().read().verify_ledger().unwrap(), head);
    assert!(matches!(
        fixture
            .invoke(
                &mut cell,
                "deliver",
                deliver(),
                "principal:operator",
                "deliver"
            )
            .unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.store().read().dump().unwrap(), before);
}
#[test]
fn inv_cp1_fifo_60_20_declared_actions_and_reopen() {
    cp1("fifo", "60", "0", "0", "1", "20");
}
#[test]
fn inv_cp1_moving_average_64_16_declared_actions_and_reopen() {
    cp1("moving_average", "64", "0.4", "6.4", "0.6", "9.6");
}

#[test]
fn inv_one_lot_fifo_delivery_has_no_placeholder_and_reopens() {
    let fixture = Fixture::load();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("single-lot.sqlite");
    let mut cell = open(&path, &fixture);
    seed_book(&mut cell, false);
    assert!(rows(&cell, "Holding").is_empty());
    assert!(rows(&cell, "Leg").is_empty());
    fixture
        .invoke(
            &mut cell,
            "createexchange",
            json!({"id":"RECEIPT-R1"}),
            "principal:operator",
            "draft-receive",
        )
        .unwrap();
    fixture
        .invoke(
            &mut cell,
            "receive",
            receive("R1", 10, 2, "fifo"),
            "principal:operator",
            "receive",
        )
        .unwrap();
    exact(&cell, "Holding", "R1", "quantity", "10");
    exact(&cell, "Holding", "R1", "value", "20");
    fixture
        .invoke(
            &mut cell,
            "createexchange",
            json!({"id":"DELIVERY"}),
            "principal:operator",
            "draft-deliver",
        )
        .unwrap();
    let input = json!({"id":"DELIVERY","holding":"R1","allocation":"DELIVERY-ALLOCATION","company":"BUSINESS","party":"PARTY","event":"DELIVERY-EVENT","debit":"DELIVERY-COGS","credit":"DELIVERY-STOCK","quantity":3});
    let mut overdraw = input.clone();
    overdraw["quantity"] = json!(11);
    assert_eq!(
        refuses(
            &fixture,
            &mut cell,
            "deliver_single",
            overdraw,
            "principal:operator",
            "single-overdraw"
        )
        .code(),
        "decision.guard_false"
    );
    fixture
        .invoke(
            &mut cell,
            "deliver_single",
            input.clone(),
            "principal:operator",
            "deliver",
        )
        .unwrap();
    exact(&cell, "Holding", "R1", "quantity", "7");
    exact(&cell, "Holding", "R1", "value", "14");
    exact(&cell, "Allocation", "DELIVERY-ALLOCATION", "quantity", "3");
    exact(&cell, "Allocation", "DELIVERY-ALLOCATION", "value", "6");
    assert_eq!(rows(&cell, "Holding").len(), 1);
    assert_eq!(rows(&cell, "Allocation").len(), 1);
    assert_eq!(rows(&cell, "Event").len(), 2);
    assert!(!cell.load(&key("Holding", "R2")).unwrap().exists());
    accounts(
        &cell,
        &[
            ("Inventory", "14"),
            ("ReceivedNotBilled", "-20"),
            ("COGS", "6"),
        ],
    );
    assert!(
        cell.load_link(&LinkKey::new(
            key("Allocation", "DELIVERY-ALLOCATION"),
            "source",
            key("Holding", "R1")
        ))
        .unwrap()
        .exists()
    );
    let dump = cell.store().read().dump().unwrap();
    drop(cell);
    let mut cell = open(&path, &fixture);
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert!(matches!(
        fixture
            .invoke(
                &mut cell,
                "deliver_single",
                input.clone(),
                "principal:operator",
                "deliver"
            )
            .unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    let mut changed = input;
    changed["quantity"] = json!(4);
    assert_eq!(
        refuses(
            &fixture,
            &mut cell,
            "deliver_single",
            changed,
            "principal:operator",
            "deliver"
        )
        .code(),
        "action.idempotency_conflict"
    );
}

#[test]
fn inv_stock_scope_and_principal_are_authoritative() {
    let fixture = Fixture::load();
    assert!(
        fixture.laws.is_empty(),
        "stock closure does not import Conversion"
    );
    let directory = tempfile::tempdir().unwrap();
    let mut cell = open(&directory.path().join("scope.sqlite"), &fixture);
    seed_book(&mut cell, false);
    let before = cell.store().read().dump().unwrap();
    refuses(
        &fixture,
        &mut cell,
        "createexchange",
        json!({"id":"FOREIGN"}),
        "principal:unprivileged",
        "unprivileged",
    );
    let error = fixture
        .invoke_scoped(
            &mut cell.host,
            "createexchange",
            json!({"id":"CROSS-SCOPE"}),
            "principal:operator",
            "cross-scope",
            BTreeMap::from([
                ("orgId".into(), "other_org".into()),
                ("environmentId".into(), "dev".into()),
            ]),
        )
        .unwrap_err();
    assert_eq!(error.code(), "action.permission_denied");
    assert_eq!(cell.store().read().dump().unwrap(), before);
    let reopened = open(&cell.path, &fixture);
    assert_eq!(reopened.store().read().dump().unwrap(), before);
}

/// INV-TRANSFER / INV-CP1 / INV-OVERDRAW / INV-REOPEN: one real stock workflow.
#[test]
fn inv_receive_transfer_fifo_sell_refuses_over_delivery_and_reopens() {
    let fixture = Fixture::load();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("workflow.sqlite");
    let mut cell = open(&path, &fixture);
    seed_book(&mut cell, false);
    for (id, company) in [("B", "BUSINESS"), ("OTHER", "PARTY")] {
        fixture
            .invoke(
                &mut cell,
                "createvault",
                json!({"id":id,"company":company}),
                "principal:operator",
                &format!("warehouse-{id}"),
            )
            .unwrap();
    }
    for (id, quantity, rate) in [("R1", 2, 10), ("R2", 3, 20)] {
        fixture
            .invoke(
                &mut cell,
                "createexchange",
                json!({"id":format!("RECEIPT-{id}")}),
                "principal:operator",
                &format!("draft-{id}"),
            )
            .unwrap();
        fixture
            .invoke(
                &mut cell,
                "receive",
                receive(id, quantity, rate, "fifo"),
                "principal:operator",
                &format!("receive-{id}"),
            )
            .unwrap();
    }
    fixture
        .invoke(
            &mut cell,
            "createtransfer",
            json!({"id":"MOVE"}),
            "principal:operator",
            "draft-move",
        )
        .unwrap();
    let movement = json!({"id":"MOVE","holding":"R1","output":"MOVE-R1","source":"VAULT","destination":"B","company":"BUSINESS","event":"MOVE-EVENT","quantity":2});
    for (field, value, operation) in [
        ("quantity", json!(3), "transfer-insufficient"),
        ("source", json!("B"), "transfer-nonmember"),
        ("destination", json!("VAULT"), "transfer-same-location"),
        ("destination", json!("OTHER"), "transfer-other-company"),
    ] {
        let mut invalid = movement.clone();
        invalid[field] = value;
        assert_eq!(
            refuses(
                &fixture,
                &mut cell,
                "transfer",
                invalid,
                "principal:operator",
                operation
            )
            .code(),
            "decision.guard_false"
        );
    }
    let books = rows(&cell, "Leg");
    let ActionOutcome::Committed {
        receipt: moved_receipt,
        record: moved_record,
    } = fixture
        .invoke(
            &mut cell,
            "transfer",
            movement.clone(),
            "principal:operator",
            "move",
        )
        .unwrap()
    else {
        panic!("transfer commits once")
    };
    assert_eq!(
        rows(&cell, "Leg"),
        books,
        "custody creates no sales, receivable or COGS effects"
    );
    accounts(&cell, &[("Inventory", "80"), ("ReceivedNotBilled", "-80")]);
    inventory(&cell, "5", "80", "0", "5");
    exact(&cell, "Holding", "R1", "quantity", "0");
    exact(&cell, "Holding", "R1", "value", "0");
    exact(&cell, "Holding", "MOVE-R1", "quantity", "2");
    exact(&cell, "Holding", "MOVE-R1", "value", "20");
    assert_eq!(
        values(&cell, "Holding", "MOVE-R1")["origin_layer"].as_str(),
        Some("R1")
    );
    assert_eq!(
        values(&cell, "Event", "MOVE-EVENT")["action"].as_str(),
        Some("transfer_custody")
    );
    for (source_kind, source, link, target_kind, target) in [
        ("Holding", "MOVE-R1", "origin", "Holding", "R1"),
        ("Holding", "MOVE-R1", "vault", "Vault", "B"),
        ("Vault", "B", "holdings", "Holding", "MOVE-R1"),
        ("Event", "MOVE-EVENT", "from_vault", "Vault", "VAULT"),
        ("Event", "MOVE-EVENT", "to_vault", "Vault", "B"),
    ] {
        assert!(
            cell.load_link(&LinkKey::new(
                key(source_kind, source),
                link,
                key(target_kind, target)
            ))
            .unwrap()
            .exists()
        );
    }
    state(&cell, "Transfer", "MOVE", "posted");
    let moved_export = cell
        .store()
        .export_record(moved_receipt.seq.0)
        .unwrap()
        .unwrap();
    let moved = cell.store().read().dump().unwrap();
    drop(cell);
    let mut cell = open(&path, &fixture);
    assert_eq!(cell.store().read().dump().unwrap(), moved);
    assert_eq!(
        fixture
            .invoke(
                &mut cell,
                "transfer",
                movement,
                "principal:operator",
                "move"
            )
            .unwrap(),
        ActionOutcome::Replayed {
            record: moved_record
        }
    );
    assert_eq!(cell.store().read().dump().unwrap(), moved);
    fixture
        .invoke(
            &mut cell,
            "createexchange",
            json!({"id":"DELIVERY"}),
            "principal:operator",
            "draft-sale",
        )
        .unwrap();
    let mut sale = deliver();
    sale["first"] = json!("MOVE-R1");
    let mut excessive = sale.clone();
    excessive["quantity"] = json!(6);
    refuses(
        &fixture,
        &mut cell,
        "sell",
        excessive,
        "principal:operator",
        "workflow-overdraw",
    );
    let ActionOutcome::Committed {
        record: sold_record,
        ..
    } = fixture
        .invoke(
            &mut cell,
            "sell",
            sale.clone(),
            "principal:operator",
            "sell",
        )
        .unwrap()
    else {
        panic!("sale commits once")
    };
    exact(&cell, "Leg", "DELIVERY-COGS", "amount", "60");
    exact(&cell, "Leg", "DELIVERY-STOCK", "amount", "-60");
    exact(&cell, "Holding", "R2", "quantity", "1");
    exact(&cell, "Holding", "R2", "value", "20");
    inventory(&cell, "1", "20", "0", "1");
    accounts(
        &cell,
        &[
            ("Inventory", "20"),
            ("ReceivedNotBilled", "-80"),
            ("COGS", "60"),
        ],
    );
    assert!(cell.store().read().pending_commands().unwrap().is_empty());
    let final_dump = cell.store().read().dump().unwrap();
    let head = cell.store().read().verify_ledger().unwrap();
    drop(cell);
    let mut cell = open(&path, &fixture);
    assert_eq!(cell.store().read().dump().unwrap(), final_dump);
    assert_eq!(cell.store().read().verify_ledger().unwrap(), head);
    assert_eq!(
        cell.store()
            .export_record(moved_receipt.seq.0)
            .unwrap()
            .unwrap(),
        moved_export
    );
    assert_eq!(
        fixture
            .invoke(
                &mut cell,
                "sell",
                sale.clone(),
                "principal:operator",
                "sell"
            )
            .unwrap(),
        ActionOutcome::Replayed {
            record: sold_record
        }
    );
    assert_eq!(cell.store().read().dump().unwrap(), final_dump);
    sale["quantity"] = json!(3);
    assert_eq!(
        refuses(
            &fixture,
            &mut cell,
            "sell",
            sale,
            "principal:operator",
            "sell"
        )
        .code(),
        "action.idempotency_conflict"
    );
}
