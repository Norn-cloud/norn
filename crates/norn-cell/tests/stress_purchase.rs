//! STRESS-PURCHASE: real SQLite commits retain workflow and inspection laws.
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
type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
struct Allow;
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
fn audit_type(mut object: ObjectTypeDef) -> ObjectTypeDef {
    object.name = norn_ir::ObjectTypeName(ACTION_CALL_TYPE.into());
    object.identity.clear();
    object.implements.clear();
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
    })
    .collect();
    object
}

fn artifacts() -> Vec<ArtifactEnvelope> {
    serde_json::from_str(include_str!("../../../fixtures/stress/purchase.json"))
        .expect("generated purchase")
}
fn run(
    action_id: &str,
    input: Value,
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    artifacts: &[ArtifactEnvelope],
    key: &str,
) -> Result<ActionOutcome, norn_kernel::ActionRefusal<<Cell as CellHost>::Error>> {
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
            input: serde_json::from_value(input).expect("input"),
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
}
#[test]
fn purchase_workflow_and_inspection_refuse_without_partial_commits() {
    let artifacts = artifacts();
    let interface: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.Process.interface.json"
    ))
    .expect("core Process");
    let payload = serde_json::to_value(interface.payload.to_json_value()).expect("payload");
    let interface: InterfaceDef =
        serde_json::from_value(payload["interface"].clone()).expect("interface");
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
    definitions.push(audit_type(definitions.first().expect("purchase").clone()));
    let model = Model::from_definitions_with_interfaces(&definitions, &[], &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("laws");
    let catalog = TypeCatalog::new(definitions.iter())
        .with_interfaces(&interfaces)
        .with_governing_laws(&artifacts);
    let mut cell = Cell::new(
        CellStore::open_in_memory(model).expect("SQLite"),
        "org_demo/dev",
        |c| c.payload.clone(),
    )
    .expect("cell");
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
    let mutant: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
        "../../../fixtures/stress/purchase-mutant.json"
    ))
    .expect("generated mutant");
    let before = cell.store().read().dump().expect("before mutant");
    let refusal = run(
        "stress.receipt.inspect.action",
        json!({"id":"r2","delivered":4,"accepted":3}),
        &mut cell,
        &catalog,
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
    let base = vec![
        serde_json::from_str(include_str!(
            "../../../registry/core/artifacts/core.Process.interface.json"
        ))
        .expect("base"),
        serde_json::from_str(include_str!(
            "../../../registry/core/artifacts/core.units.json"
        ))
        .expect("units"),
    ];
    let diagnostics = norn_checker::check_artifact_set(
        &artifacts(),
        &norn_checker::EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.severity == norn_ir::DiagnosticSeverity::Error),
        "{diagnostics:#?}"
    );
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
    assert!(workflow.goals.iter().all(|g| {
        g.as_ref()
            .is_err_and(|e| e == "only create-only input writers supported")
    }));
}
