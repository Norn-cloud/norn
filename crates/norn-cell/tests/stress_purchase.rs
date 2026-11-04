//! STRESS-PURCHASE: real `SQLite` commits retain workflow and inspection laws.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "named acceptance scenarios"
)]
use norn_cell::{CellStore, Model, SqliteCell};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRootV2, InterfaceDef, ObjectTypeDef, Property,
    PropertyConstraints, PropertyId, PropertyType,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, CellHost, DecisionTurn, FactEvent, LogicalTime, TypeCatalog, World,
    commit_action,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub(crate) type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
pub(crate) struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize_read(
        &self,
        _: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> std::result::Result<(), ActionDenial> {
        Ok(())
    }
}

// The host's existing action idempotency record is infrastructure, not a business writer.
fn audit_type() -> ObjectTypeDef {
    let mut object: ObjectTypeDef =
        serde_json::from_value(json!({"name": ACTION_CALL_TYPE})).expect("audit type");
    object.properties = [
        "actionId",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "rowOperations",
    ]
    .into_iter()
    .map(|id| Property {
        id: PropertyId(id.into()),
        description: None,
        property_type: PropertyType::JsonExt { schema: None },
        constraints: PropertyConstraints {
            nullable: true,
            ..PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
        protected: false,
    })
    .collect();
    object
}

pub(crate) fn artifacts() -> Vec<ArtifactEnvelope> {
    serde_json::from_str(include_str!("../../../fixtures/stress/purchase.json"))
        .expect("generated purchase")
}
fn action_input(
    artifacts: &[ArtifactEnvelope],
    action_id: &str,
    input: Value,
) -> (norn_ir::ActionRoot, JsonObject) {
    let artifact = artifacts
        .iter()
        .find(|a| a.artifact_id.as_str() == action_id)
        .expect("action");
    let ArtifactPayload::Action(payload) = &artifact.payload else {
        panic!("action payload")
    };
    let action = payload
        .to_json_value()
        .decode::<norn_ir::ActionRoot>()
        .expect("action IR");
    (action, serde_json::from_value(input).expect("input"))
}
pub(crate) fn run(
    action_id: &str,
    input: Value,
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    artifacts: &[ArtifactEnvelope],
    key: &str,
) -> Result<ActionOutcome, Box<norn_kernel::ActionRefusal<<Cell as CellHost>::Error>>> {
    let (action, input) = action_input(artifacts, action_id, input);
    commit_action(
        cell,
        catalog,
        &ActionCall {
            action: &action,
            action_artifact_id: action_id,
            action_version: "1.0.0",
            scope: BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]),
            actor: "synthetic:operator".into(),
            input,
            idempotency_key: key.into(),
            object_type: None,
            turn: DecisionTurn {
                id: key.into(),
                now: LogicalTime::from_millis(1),
                world: World::Main,
                consumes: None,
            },
        },
        &Allow,
    )
    .map_err(Box::new)
}
pub(crate) fn setup(
    artifacts: &[ArtifactEnvelope],
) -> (Cell, Vec<ObjectTypeDef>, [InterfaceDef; 1]) {
    let interface: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.Process.interface.json"
    ))
    .expect("core Process");
    let payload = serde_json::to_value(interface.payload.to_json_value()).expect("payload");
    let interface: InterfaceDef =
        serde_json::from_value(payload.get("interface").expect("interface body").clone())
            .expect("interface");
    let interfaces = [interface];
    let mut definitions: Vec<ObjectTypeDef> = artifacts
        .iter()
        .filter_map(|a| {
            if let ArtifactPayload::Entity(payload) = &a.payload {
                Some(
                    payload
                        .to_json_value()
                        .decode::<EntityRootV2>()
                        .expect("entity")
                        .object_type,
                )
            } else {
                None
            }
        })
        .collect();
    definitions.push(audit_type());
    let model = Model::from_definitions_with_interfaces(&definitions, &[], &interfaces)
        .expect("model")
        .with_governing_laws(artifacts)
        .expect("laws");
    let cell = Cell::new(
        CellStore::open_in_memory(model).expect("SQLite"),
        "org_demo/dev",
        |c| c.payload.clone(),
    )
    .expect("cell");
    (cell, definitions, interfaces)
}
#[test]
fn purchase_workflow_and_inspection_refuse_without_partial_commits() {
    let artifacts = artifacts();
    let (mut cell, definitions, interfaces) = setup(&artifacts);
    let catalog = TypeCatalog::new(definitions.iter())
        .with_interfaces(&interfaces)
        .with_governing_laws(&artifacts);
    let before = cell.store().read().dump().expect("before zero order");
    let error = run(
        "stress.purchase.create.action",
        json!({"id":"zero","ordered":0}),
        &mut cell,
        &catalog,
        &artifacts,
        "zero",
    )
    .expect_err("positive order law");
    assert_eq!(error.code(), "core.laws.violated");
    assert!(
        error
            .to_string()
            .contains("stress.purchase_positive: A purchase promises a positive quantity.")
    );
    assert_eq!(before, cell.store().read().dump().expect("atomic refusal"));
    for (action, input, key) in [
        (
            "stress.purchase.create.action",
            json!({"id":"p1","ordered":4}),
            "create",
        ),
        (
            "stress.purchase.approve.action",
            json!({"id":"p1"}),
            "approve",
        ),
        (
            "stress.purchase.record_received.action",
            json!({"id":"p1","received":4}),
            "record",
        ),
        (
            "stress.purchase.receive.action",
            json!({"id":"p1"}),
            "receive",
        ),
        (
            "stress.receipt.inspect.action",
            json!({"id":"r1","delivered":4,"accepted":3}),
            "inspect",
        ),
    ] {
        assert!(matches!(
            run(action, input, &mut cell, &catalog, &artifacts, key)
                .unwrap_or_else(|e| panic!("{action}: {e:?}")),
            ActionOutcome::Committed { .. }
        ));
        if key == "approve" {
            for received in [-1, 5] {
                let before = cell.store().read().dump().expect("before invalid receipt");
                let error = run(
                    "stress.purchase.record_received.action",
                    json!({"id":"p1","received":received}),
                    &mut cell,
                    &catalog,
                    &artifacts,
                    &format!("invalid-{received}"),
                )
                .expect_err("receipt limit law");
                assert_eq!(error.code(), "core.laws.violated");
                assert!(error.to_string().contains("stress.purchase_receipt_limit: Received quantity cannot be negative or exceed the approved order."));
                assert_eq!(before, cell.store().read().dump().expect("atomic refusal"));
            }
        }
        if key == "create" {
            let before = cell.store().read().dump().expect("before transition");
            let refusal = run(
                "stress.purchase.receive.action",
                json!({"id":"p1"}),
                &mut cell,
                &catalog,
                &artifacts,
                "premature",
            )
            .expect_err("approval required");
            assert_eq!(refusal.code(), norn_ir::lifecycle_codes::UNDECLARED_WRITE);
            assert!(
                refusal
                    .to_string()
                    .contains("transition receive only leaves approved")
            );
            assert_eq!(before, cell.store().read().dump().expect("atomic refusal"));
        }
    }
    assert_inspection_mutant_atomic(&mut cell, &catalog);
}

fn assert_inspection_mutant_atomic(cell: &mut Cell, catalog: &TypeCatalog<'_>) {
    let mutant: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
        "../../../fixtures/stress/purchase-mutant.json"
    ))
    .expect("generated mutant");
    let before = cell.store().read().dump().expect("before mutant");
    let refusal = run(
        "stress.receipt.inspect.action",
        json!({"id":"r2","delivered":4,"accepted":3}),
        cell,
        catalog,
        &mutant,
        "mutant",
    )
    .expect_err("unaccounted rejection");
    assert_eq!(refusal.code(), "core.laws.violated");
    assert!(refusal.to_string().contains("stress.receipt_balance: Inspection accounts for the entire delivery, including rejected goods."));
    assert_eq!(
        before,
        cell.store().read().dump().expect("mutant atomic refusal")
    );
}

#[test]
fn complete_contract_keeps_workflow_unknowns_and_generates_receipt_goals() {
    use norn_checker::obligations::{ApprovedContract, DefinitionSource, quint::Bounds};
    use std::collections::BTreeSet;
    let source = DefinitionSource::parse(
        include_str!("../../../fixtures/stress/purchase.json"),
        &format!(
            "[{},{}]",
            include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
            include_str!("../../../registry/core/artifacts/core.units.json")
        ),
    )
    .expect("source");
    let registry = BTreeSet::new();
    let authority = ApprovedContract::approve(
        &source,
        &registry,
        json!({"expression":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"bounded"}),
    )
    .expect("admission");
    let bounds = Bounds {
        action: "stress.receipt.inspect.action".into(),
        identity: "receipt-one".into(),
        inputs: BTreeMap::from([
            ("delivered".into(), ("1".into(), "4".into())),
            ("accepted".into(), ("0".into(), "4".into())),
        ]),
        seed: 0,
    };
    let batch = authority
        .quint(&source, &registry, &bounds)
        .expect("generated inventory");
    assert_eq!(batch.goals.iter().filter(|g| g.is_ok()).count(), 3);
    assert!(batch.goals.iter().any(|g| {
        g.as_ref()
            .is_err_and(|e| e.contains("outside selected one-create pointwise relation"))
    }));
    let workflow = authority
        .quint(
            &source,
            &registry,
            &Bounds {
                action: "stress.purchase.approve.action".into(),
                ..bounds
            },
        )
        .expect("workflow inventory");
    let inventory = workflow
        .challenge
        .body()
        .get("obligations")
        .and_then(Value::as_array)
        .expect("workflow obligations");
    assert_eq!(workflow.goals.len(), inventory.len());
    assert_eq!(workflow.goals.iter().filter(|goal| goal.is_ok()).count(), 0);
    let mut lifecycle_goals = 0;
    for (goal, result) in inventory.iter().zip(&workflow.goals) {
        let kind = goal
            .get("goal")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .expect("goal kind");
        let expected = match kind {
            "lifecycle_terminal_reachability"
            | "lifecycle_no_stuck_non_terminal"
            | "lifecycle_transition_non_vacuity" => {
                lifecycle_goals += 1;
                assert_eq!(
                    goal.get("assessment").and_then(Value::as_str),
                    Some("unknown")
                );
                assert_eq!(
                    goal.get("reason").and_then(Value::as_str),
                    Some(
                        "conditional finite lifecycle goal requires a qualified bounds profile and checked result"
                    )
                );
                "generated goal has no governing law identity"
            }
            "initialization" | "non_vacuity" | "law_semantics" | "preservation"
            | "open_boundary" => "only create-only input writers supported",
            other => panic!("unclassified workflow goal {other}"),
        };
        assert_eq!(
            result.as_ref().err().map(String::as_str),
            Some(expected),
            "{goal}"
        );
    }
    assert_eq!(lifecycle_goals, 6);
}

#[test]
fn bounded_inspection_commit_refuse_and_exact_post_state_correspondence() {
    let original = artifacts();
    let mutant = serde_json::from_str::<Vec<ArtifactEnvelope>>(include_str!(
        "../../../fixtures/stress/purchase-mutant.json"
    ))
    .expect("mutant");
    for (declarations, mutated) in [(&original, false), (&mutant, true)] {
        for delivered in 1..=4 {
            for accepted in 0..=4 {
                let (mut cell, definitions, interfaces) = setup(declarations);
                let catalog = TypeCatalog::new(definitions.iter())
                    .with_interfaces(&interfaces)
                    .with_governing_laws(declarations);
                let before = cell.store().read().dump().expect("before");
                let result = run(
                    "stress.receipt.inspect.action",
                    json!({"id":"receipt-one","delivered":delivered,"accepted":accepted}),
                    &mut cell,
                    &catalog,
                    declarations,
                    "bounded",
                );
                if accepted <= delivered && (!mutated || accepted == delivered) {
                    assert!(matches!(
                        result.expect("valid inspection"),
                        ActionOutcome::Committed { .. }
                    ));
                    let actual = cell
                        .load(&norn_kernel::FactKey::new(
                            "stress.ReceiptLine",
                            "receipt-one",
                        ))
                        .expect("load")
                        .values
                        .expect("created inspection");
                    let expected: JsonObject = serde_json::from_value(json!({"id":"receipt-one","delivered":delivered,"accepted":accepted,"rejected":delivered-accepted})).expect("exact expected state");
                    assert_eq!(actual, expected);
                } else {
                    let refusal = result.expect_err("invalid inspection");
                    let (code, text) = if accepted > delivered {
                        ("action.precondition_failed", "accepted")
                    } else {
                        (
                            "core.laws.violated",
                            "stress.receipt_balance: Inspection accounts for the entire delivery, including rejected goods.",
                        )
                    };
                    assert_eq!(refusal.code(), code);
                    assert!(refusal.to_string().contains(text), "{refusal}");
                    assert_eq!(before, cell.store().read().dump().expect("atomic refusal"));
                }
            }
        }
    }
}
