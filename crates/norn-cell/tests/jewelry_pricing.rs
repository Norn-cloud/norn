#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "acceptance assertions"
)]

//! JEWEL-PRICE / VAL-RATE: authored USD/g pricing through an actual SQLite action.
//! This covers the pricing step, not inventory valuation or balanced GL posting.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DiagnosticSeverity, EntityRootV2, ObjectTypeDef, Property,
    PropertyConstraints, PropertyId, PropertyType, RuntimeValue,
};
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, CellHost, DecisionTurn, FactEvent, LogicalTime, TypeCatalog, World,
    commit_action,
};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
const QUOTE: &str = "jewelry.ExchangePrice";
const ARTIFACT: &str = "jewelry.exchange.price.action";

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn artifacts() -> Vec<ArtifactEnvelope> {
    serde_json::from_str(include_str!(
        "../../../tools/norn-ts/fixtures/definitions/jewelry-pricing.ir.json"
    ))
    .expect("generated typed pricing fixture")
}

fn findings(artifacts: &[ArtifactEnvelope]) -> Vec<norn_checker::Diagnostic> {
    let units = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.units.json"
    ))
    .expect("pinned unit catalog");
    norn_checker::check_artifact_set(
        artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[units],
            tenant: None,
        },
    )
}

fn pricing(artifacts: &[ArtifactEnvelope]) -> (ObjectTypeDef, norn_ir::ActionRoot) {
    let entity = artifacts
        .iter()
        .find_map(|a| {
            let ArtifactPayload::Entity(payload) = &a.payload else {
                return None;
            };
            Some(
                payload
                    .to_json_value()
                    .decode::<EntityRootV2>()
                    .expect("entity root")
                    .object_type,
            )
        })
        .expect("exchange price type");
    let action = artifacts
        .iter()
        .find_map(|a| {
            let ArtifactPayload::Action(payload) = &a.payload else {
                return None;
            };
            Some(payload.to_json_value().decode().expect("action root"))
        })
        .expect("pricing action");
    (entity, action)
}

// The generic action-call storage projection, separate from authored business definitions.
fn audit_type(mut definition: ObjectTypeDef) -> ObjectTypeDef {
    definition.name = norn_ir::ObjectTypeName(ACTION_CALL_TYPE.into());
    definition.identity.clear();
    definition.properties = [
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
    definition
}

fn number(text: &str) -> RuntimeValue {
    RuntimeValue::from_json_text(text).expect("exact decimal token")
}

fn input(id: &str, side: &str, rate: &str, grams: &str, currency: &str) -> JsonObject {
    BTreeMap::from([
        ("quote_id".into(), id.into()),
        ("side".into(), side.into()),
        ("currency".into(), currency.into()),
        ("price_per_g".into(), number(rate)),
        ("fine_g".into(), number(grams)),
    ])
}

fn call<'a>(action: &'a norn_ir::ActionRoot, input: JsonObject, request: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: ARTIFACT,
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "test:operator".into(),
        input,
        idempotency_key: request.into(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{request}"),
            now: LogicalTime::from_millis(1000),
            world: World::Main,
            consumes: None,
        },
    }
}

#[test]
fn jewel_price_sell_and_scrap_commit_exactly_once_and_reopen() {
    let artifacts = artifacts();
    let diagnostics = findings(&artifacts);
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error),
        "{diagnostics:?}"
    );
    assert!(diagnostics.iter().any(|d| d.code == "core.values.literal_unit" && d.severity == DiagnosticSeverity::Info), "{diagnostics:?}");
    let (quote, action) = pricing(&artifacts);
    let definitions = [quote.clone(), audit_type(quote)];
    let model = Model::from_definitions(&definitions, &[]).expect("projects admitted pricing");
    let catalog = TypeCatalog::new(definitions.iter().take(1));
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("pricing.sqlite");
    let mut cell = Cell::open(
        &path,
        model.clone(),
        StoreOptions::default(),
        "org_demo/dev",
        |command| command.payload.clone(),
    )
    .expect("SQLite cell");
    for (id, side, rate, expected) in [
        ("sell-20", "sell", "15.000000", "300.00"),
        ("scrap-20", "scrap", "9.000000", "180.00"),
    ] {
        let request = call(&action, input(id, side, rate, "20.000", "USD"), id);
        let result =
            commit_action(&mut cell, &catalog, &request, &Allow).expect("priced action commits");
        let ActionOutcome::Committed { record, .. } = result else {
            panic!("first intent commits")
        };
        assert_eq!(
            record.effective_input.as_ref().expect("derived input")["amount"],
            number(expected)
        );
        assert_eq!(
            cell.load(&FactKey::new(QUOTE, id))
                .expect("quote")
                .values
                .expect("stored quote")["amount"],
            number(expected)
        );
        assert!(matches!(
            commit_action(&mut cell, &catalog, &request, &Allow).expect("exact retry"),
            ActionOutcome::Replayed { .. }
        ));
    }
    let committed = cell.commits().expect("full committed ledger");
    assert_eq!(committed.len(), 2);
    drop(cell);
    let mut cell = Cell::open(
        &path,
        model,
        StoreOptions::default(),
        "org_demo/dev",
        |command| command.payload.clone(),
    )
    .expect("reopen");
    assert_eq!(cell.commits().expect("reopened ledger"), committed);
    for (id, side, rate, expected) in [
        ("sell-20", "sell", "15.000000", "300"),
        ("scrap-20", "scrap", "9.000000", "180"),
    ] {
        let stored = cell
            .load(&FactKey::new(QUOTE, id))
            .expect("reopened quote")
            .values
            .expect("quote");
        assert_eq!(stored["currency"], RuntimeValue::from("USD"));
        assert_eq!(stored["fine_g"], number("20"));
        assert_eq!(stored["price_per_g"], number(rate));
        assert_eq!(stored["amount"], number(expected));
        let retry = call(&action, input(id, side, rate, "20.000", "USD"), id);
        assert!(matches!(
            commit_action(&mut cell, &catalog, &retry, &Allow).expect("durable retry"),
            ActionOutcome::Replayed { .. }
        ));
    }
    assert_eq!(
        cell.commits().expect("no duplicate ledger entries"),
        committed
    );

    assert_refusals(&mut cell, &catalog, &action, &committed);
}

fn assert_refusals(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    action: &norn_ir::ActionRoot,
    committed: &[norn_cell::CommitRecord<FactEvent>],
) {
    for (id, currency, rate, grams, code) in [
        ("wrong-currency", "EUR", "15", "20", "action.invalid_input"),
        (
            "inexact-cent",
            "USD",
            "0.005",
            "1",
            "action.derivation_failed",
        ),
        ("zero-mass", "USD", "15", "0", "action.precondition_failed"),
    ] {
        let refused = commit_action(
            cell,
            catalog,
            &call(action, input(id, "sell", rate, grams, currency), id),
            &Allow,
        )
        .expect_err("pricing refuses");
        assert_eq!(refused.code(), code);
        assert!(
            cell.load(&FactKey::new(QUOTE, id))
                .expect("no partial quote")
                .values
                .is_none()
        );
        assert_eq!(
            cell.commits().expect("refusal leaves ledger unchanged"),
            committed
        );
        assert!(cell.pending_commands().expect("outbox").is_empty());
        assert!(cell.pending_timers().expect("timers").is_empty());
    }
}

#[test]
fn jewel_price_milligram_denominator_refuses_before_execution() {
    let mut source = serde_json::to_value(artifacts()).expect("negative conformance probe");
    let action = source
        .as_array_mut()
        .expect("artifacts")
        .iter_mut()
        .find(|a| a["kind"] == "action")
        .expect("action");
    action["payload"]["inputSchema"]["properties"]["price_per_g"]["nornType"]["perUnit"]["code"] =
        "mg".into();
    let changed: Vec<ArtifactEnvelope> =
        serde_json::from_value(source).expect("changed declaration");
    let diagnostics = findings(&changed);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "action.validation.invalid_payload"
                && d.message.contains("core.values.unit_mismatch")
                && d.severity == DiagnosticSeverity::Error),
        "{diagnostics:?}"
    );
}
