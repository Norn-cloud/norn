//! Verifies that `norn-kernel` enforces interface invariants reading
//! registry-typed (`Norn.ref`) properties at commit time (#1583).

use norn_ir::{
    ArtifactEnvelope, InterfaceDef, ObjectTypeDef, PropertyId, PropertyTypeDef, TransitionId,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, CommitError, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LinkMutation, LinkWrite,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn core_property_types() -> TestResult<Vec<PropertyTypeDef>> {
    let mut out = Vec::new();
    for raw in [
        include_str!("../../../registry/core/artifacts/core.value_types.entity.json"),
        include_str!("../../../registry/core/artifacts/std.value_types.entity.json"),
    ] {
        let artifact: Value = serde_json::from_str(raw)?;
        let types = artifact
            .get("payload")
            .and_then(|payload| payload.get("propertyTypes"))
            .ok_or("missing payload.propertyTypes")?
            .clone();
        out.extend(serde_json::from_value::<Vec<PropertyTypeDef>>(types)?);
    }
    Ok(out)
}

fn core_interfaces() -> TestResult<Vec<InterfaceDef>> {
    [
        include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Hold.interface.json"),
        include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Claim.interface.json"),
    ]
    .into_iter()
    .map(|raw| {
        let artifact: Value = serde_json::from_str(raw)?;
        let interface = artifact
            .get("payload")
            .and_then(|payload| payload.get("interface"))
            .ok_or("missing payload.interface")?
            .clone();
        Ok(serde_json::from_value::<InterfaceDef>(interface)?)
    })
    .collect()
}

fn rea_object_types() -> TestResult<Vec<ObjectTypeDef>> {
    Ok(vec![
        serde_json::from_value(json!({
            "name": "test.Holding",
            "implements": ["core.Resource"],
            "properties": [
                { "id": "resource_code", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } },
                { "id": "valuation_method", "type": { "kind": "enum", "values": [{ "value": "fifo" }, { "value": "moving_average" }] }, "constraints": { "nullable": true } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.OrderLine",
            "implements": ["core.Commitment"],
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "requested" }, { "value": "promised" }, { "value": "fulfilled" }, { "value": "cancelled" }, { "value": "breached" }] } },
                { "id": "due_at", "type": { "kind": "ref", "name": "iso8601.timestamp" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.Reservation",
            "implements": ["core.Hold"],
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "released" }, { "value": "consumed" }, { "value": "expired" }] } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } },
                { "id": "expires_at", "type": { "kind": "ref", "name": "iso8601.timestamp" }, "constraints": { "nullable": true } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.Delivery",
            "implements": ["core.EconomicEvent"],
            "properties": [
                { "id": "action", "type": { "kind": "enum", "values": [{ "value": "transfer_ownership" }, { "value": "transfer_custody" }, { "value": "consume" }, { "value": "produce" }] } },
                { "id": "occurred_at", "type": { "kind": "ref", "name": "iso8601.timestamp" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.Posting",
            "implements": ["core.LedgerEffect"],
            "properties": [
                { "id": "account_code", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "amount", "type": { "kind": "ref", "name": "core.money_amount" } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.Receivable",
            "implements": ["core.Claim"],
            "properties": [
                { "id": "amount", "type": { "kind": "ref", "name": "core.money_amount" } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "due_date", "type": { "kind": "ref", "name": "iso8601.date" } }
            ]
        }))?,
    ])
}

fn rea_cell() -> TestResult<Cell> {
    Ok(Cell::new(|command| command.payload.clone())
        .with_types(rea_object_types()?)
        .with_interfaces(core_interfaces()?)
        .with_property_types(core_property_types()?))
}

fn put(cell: &mut Cell, object_type: &str, key: &str, values: Value) -> Result<(), String> {
    let values: JsonObject = serde_json::from_value(values).map_err(|err| err.to_string())?;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(object_type, key),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values },
        }],
        ..CellCommit::empty()
    })
    .map(|_| ())
    .map_err(|err| err.code().to_owned())
}

#[test]
fn core_rea_invariants_admit_valid_records_at_commit() -> TestResult {
    let mut cell = rea_cell()?;

    // Valid records: depleted resource (0), monetary resource, positive commitment/hold/event,
    // signed credit & debit ledger effects, positive claim.
    let valid_cases = [
        (
            "test.Holding",
            "h-zero",
            json!({
                "resource_code": "GOLD-1",
                "quantity": 0,
                "unit": "g",
                "currency": null,
                "valuation_method": null
            }),
        ),
        (
            "test.Holding",
            "h-cash",
            json!({
                "resource_code": "CASH-USD",
                "quantity": 100.5,
                "unit": null,
                "currency": "USD",
                "valuation_method": null
            }),
        ),
        (
            "test.OrderLine",
            "ol-1",
            json!({
                "status": "promised",
                "due_at": "2026-12-01T00:00:00Z",
                "quantity": 5,
                "unit": "g",
                "currency": null
            }),
        ),
        (
            "test.Reservation",
            "res-1",
            json!({
                "status": "open",
                "quantity": 2,
                "unit": "g",
                "currency": null,
                "expires_at": null
            }),
        ),
        (
            "test.Delivery",
            "del-1",
            json!({
                "action": "transfer_ownership",
                "occurred_at": "2026-11-23T12:00:00Z",
                "quantity": 1,
                "unit": "g",
                "currency": null
            }),
        ),
        (
            "test.Posting",
            "post-cr",
            json!({
                "account_code": "4000",
                "amount": -900,
                "currency": "USD"
            }),
        ),
        (
            "test.Posting",
            "post-dr",
            json!({
                "account_code": "1100",
                "amount": 900,
                "currency": "USD"
            }),
        ),
        (
            "test.Receivable",
            "inv-1",
            json!({
                "amount": 250.25,
                "currency": "USD",
                "due_date": "2026-12-31"
            }),
        ),
    ];

    for (object_type, key, values) in valid_cases {
        assert_eq!(
            put(&mut cell, object_type, key, values),
            Ok(()),
            "{object_type}/{key} should admit"
        );
    }
    Ok(())
}

#[test]
fn core_rea_invariants_refuse_violations_at_commit() -> TestResult {
    let mut cell = rea_cell()?;

    // Violations must refuse with commit.invariant_violation and leave no fact committed.
    let bad_cases = [
        (
            "test.Holding",
            "bad-neg",
            json!({"resource_code":"GOLD-NEG","quantity":-1,"unit":"g","currency":null,"valuation_method":null}),
        ),
        (
            "test.Holding",
            "bad-both",
            json!({"resource_code":"GOLD-BOTH","quantity":10,"unit":"g","currency":"USD","valuation_method":null}),
        ),
        (
            "test.Holding",
            "bad-neither",
            json!({"resource_code":"GOLD-NONE","quantity":10,"unit":null,"currency":null,"valuation_method":null}),
        ),
        (
            "test.OrderLine",
            "bad-zero",
            json!({"status":"promised","due_at":"2026-12-01T00:00:00Z","quantity":0,"unit":"g","currency":null}),
        ),
        (
            "test.Reservation",
            "bad-zero",
            json!({"status":"open","quantity":0,"unit":"g","currency":null,"expires_at":null}),
        ),
        (
            "test.Delivery",
            "bad-zero",
            json!({"action":"transfer_ownership","occurred_at":"2026-11-23T12:00:00Z","quantity":0,"unit":"g","currency":null}),
        ),
        (
            "test.Posting",
            "bad-zero",
            json!({"account_code":"4000","amount":0,"currency":"USD"}),
        ),
        (
            "test.Receivable",
            "bad-zero",
            json!({"amount":0,"currency":"USD","due_date":"2026-12-31"}),
        ),
        (
            "test.Receivable",
            "bad-neg",
            json!({"amount":-5,"currency":"USD","due_date":"2026-12-31"}),
        ),
    ];

    for (object_type, key, values) in bad_cases {
        let Err(err) = put(&mut cell, object_type, key, values) else {
            return Err(format!("{object_type}/{key} must refuse").into());
        };
        assert_eq!(
            err, "commit.invariant_violation",
            "{object_type}/{key} expected commit.invariant_violation"
        );
        assert!(
            !cell
                .load(&FactKey::new(object_type, key))
                .map_err(|load_err| format!("load {object_type}/{key}: {load_err:?}"))?
                .exists(),
            "{object_type}/{key} must not persist after refusal"
        );
    }
    Ok(())
}

#[test]
fn core_rea_single_measure_handles_omitted_nullable_keys() -> TestResult {
    let mut cell = rea_cell()?;

    // Omitting nullable `currency` while supplying `unit` satisfies single_measure.
    assert_eq!(
        put(
            &mut cell,
            "test.Holding",
            "h-unit-only",
            json!({
                "resource_code": "GOLD-UNIT-ONLY",
                "quantity": 5,
                "unit": "g"
            }),
        ),
        Ok(())
    );

    // Omitting nullable `unit` while supplying `currency` satisfies single_measure.
    assert_eq!(
        put(
            &mut cell,
            "test.Holding",
            "h-currency-only",
            json!({
                "resource_code": "CASH-CURRENCY-ONLY",
                "quantity": 100,
                "currency": "USD"
            }),
        ),
        Ok(())
    );

    // Omitting both nullable `unit` and `currency` violates single_measure.
    let Err(err) = put(
        &mut cell,
        "test.Holding",
        "h-omitted-both",
        json!({
            "resource_code": "GOLD-OMITTED-BOTH",
            "quantity": 5
        }),
    ) else {
        return Err("omitting both unit and currency must refuse".into());
    };
    assert_eq!(err, "commit.invariant_violation");
    assert!(
        !cell
            .load(&FactKey::new("test.Holding", "h-omitted-both"))
            .map_err(|load_err| format!("load h-omitted-both: {load_err:?}"))?
            .exists(),
        "refused omitted-measure holding must not persist"
    );
    Ok(())
}

fn core_physics_interfaces() -> TestResult<Vec<InterfaceDef>> {
    let mut interfaces = core_interfaces()?;
    for raw in [
        include_str!("../../../registry/core/artifacts/core.HoldableResource.interface.json"),
        include_str!("../../../registry/core/artifacts/core.ValuedEvent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.ValuedLedgerEffect.interface.json"),
        include_str!("../../../registry/core/artifacts/core.ReversibleEvent.interface.json"),
    ] {
        let artifact: Value = serde_json::from_str(raw)?;
        let interface = artifact
            .get("payload")
            .and_then(|payload| payload.get("interface"))
            .ok_or("missing payload.interface")?
            .clone();
        interfaces.push(serde_json::from_value::<InterfaceDef>(interface)?);
    }
    Ok(interfaces)
}

fn core_physics_laws() -> TestResult<Vec<ArtifactEnvelope>> {
    [
        include_str!(
            "../../../registry/core/artifacts/core.Hold.measure_matches_resource.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.EconomicEvent.reversal_matches_original.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.ReversibleEvent.no_reversal_of_reversal.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.ReversibleEvent.reversal_flag_requires_link.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.HoldableResource.open_holds_within_quantity.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.ValuedEvent.effects_balance.law.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.ValuedLedgerEffect.currency_matches_event.law.json"
        ),
    ]
    .into_iter()
    .map(|raw| Ok(serde_json::from_str::<ArtifactEnvelope>(raw)?))
    .collect()
}

fn core_physics_object_types() -> TestResult<Vec<ObjectTypeDef>> {
    Ok(vec![
        serde_json::from_value(json!({
            "name": "test.HoldableHolding",
            "implements": ["core.HoldableResource"],
            "properties": [
                { "id": "resource_code", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } },
                { "id": "valuation_method", "type": { "kind": "enum", "values": [{ "value": "fifo" }, { "value": "moving_average" }] }, "constraints": { "nullable": true } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.EscrowHold",
            "implements": ["core.Hold"],
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "released" }, { "value": "consumed" }, { "value": "expired" }] } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } },
                { "id": "expires_at", "type": { "kind": "ref", "name": "iso8601.timestamp" }, "constraints": { "nullable": true } }
            ],
            "links": [
                { "id": "resource", "target": "test.HoldableHolding", "cardinality": { "to": "one", "from": "many", "required": true }, "inverse": "holds" }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.ValuedDelivery",
            "implements": ["core.ValuedEvent", "core.ReversibleEvent"],
            "properties": [
                { "id": "action", "type": { "kind": "enum", "values": [{ "value": "transfer_ownership" }, { "value": "transfer_custody" }, { "value": "consume" }, { "value": "produce" }] } },
                { "id": "occurred_at", "type": { "kind": "ref", "name": "iso8601.timestamp" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } },
                { "id": "valuation_currency", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "is_reversal", "type": { "kind": "boolean" } }
            ],
            "links": [
                { "id": "resource", "target": "test.HoldableHolding", "cardinality": { "to": "one", "from": "many", "required": true } },
                { "id": "reverses", "target": "test.ValuedDelivery", "cardinality": { "to": "one", "from": "one" } }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.ValuedPosting",
            "implements": ["core.ValuedLedgerEffect"],
            "properties": [
                { "id": "account_code", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "amount", "type": { "kind": "ref", "name": "core.money_amount" } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" } }
            ],
            "links": [
                { "id": "event", "target": "test.ValuedDelivery", "cardinality": { "to": "one", "from": "many", "required": true }, "inverse": "effects" }
            ]
        }))?,
        serde_json::from_value(json!({
            "name": "test.OrderLine",
            "implements": ["core.Commitment"],
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "requested" }, { "value": "promised" }, { "value": "fulfilled" }, { "value": "cancelled" }, { "value": "breached" }] } },
                { "id": "due_at", "type": { "kind": "ref", "name": "iso8601.timestamp" } },
                { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } },
                { "id": "unit", "type": { "kind": "ref", "name": "ucum.unit_code" }, "constraints": { "nullable": true } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" }, "constraints": { "nullable": true } }
            ]
        }))?,
    ])
}

fn core_physics_cell() -> TestResult<Cell> {
    Ok(Cell::new(|command| command.payload.clone())
        .with_types(core_physics_object_types()?)
        .with_interfaces(core_physics_interfaces()?)
        .with_property_types(core_property_types()?)
        .with_governing_laws(core_physics_laws()?))
}

fn put_write(
    object_type: &str,
    key: &str,
    expected: FactVersion,
    values: Value,
) -> TestResult<FactWrite> {
    Ok(FactWrite {
        key: FactKey::new(object_type, key),
        expected,
        mutation: FactMutation::Put {
            values: serde_json::from_value(values)?,
        },
    })
}

fn add_link(from: &FactKey, link: &str, to: &FactKey) -> LinkWrite {
    LinkWrite {
        key: LinkKey::new(from.clone(), link, to.clone()),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    }
}

fn transitioned(key: &FactKey, transition: &str, from: &str, to: &str) -> FactEvent {
    FactEvent::Transitioned {
        key: key.clone(),
        lifecycle: PropertyId("status".to_owned()),
        transition: TransitionId(transition.to_owned()),
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

fn violated_law_id<T, E>(result: &Result<T, CommitError<E>>) -> Option<&str> {
    match result {
        Err(CommitError::Law {
            reason: norn_kernel::governing_laws::LawEvaluationError::Violated(failure),
            ..
        }) => Some(failure.law_id.as_str()),
        _ => None,
    }
}

#[test]
fn core_physics_hold_escrow_measure_and_lifecycle_enforce_at_commit() -> TestResult {
    let mut cell = core_physics_cell()?;
    let res = FactKey::new("test.HoldableHolding", "h-1");
    let hold1 = FactKey::new("test.EscrowHold", "hold-1");
    let hold2 = FactKey::new("test.EscrowHold", "hold-2");
    let bad_unit = FactKey::new("test.EscrowHold", "hold-bad-unit");

    cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.HoldableHolding",
                "h-1",
                FactVersion::ABSENT,
                json!({"resource_code":"GOLD-1","quantity":100,"unit":"g","currency":null,"valuation_method":null}),
            )?,
            put_write(
                "test.EscrowHold",
                "hold-1",
                FactVersion::ABSENT,
                json!({"status":"open","quantity":60,"unit":"g","currency":null,"expires_at":null}),
            )?,
        ],
        links: vec![add_link(&hold1, "resource", &res)],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("first 60-unit hold must admit: {err:?}"))?;

    // A hold with mismatched unit against its resource is refused by core.Hold.measure_matches_resource.
    let unit_refusal = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.EscrowHold",
            "hold-bad-unit",
            FactVersion::ABSENT,
            json!({"status":"open","quantity":10,"unit":"kg","currency":null,"expires_at":null}),
        )?],
        links: vec![add_link(&bad_unit, "resource", &res)],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&unit_refusal),
        Some("core.Hold.measure_matches_resource")
    );

    // Second 60-unit open hold exceeds 100 balance (60 + 60 > 100) and is refused.
    let escrow_refusal = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.EscrowHold",
            "hold-2",
            FactVersion::ABSENT,
            json!({"status":"open","quantity":60,"unit":"g","currency":null,"expires_at":null}),
        )?],
        links: vec![add_link(&hold2, "resource", &res)],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&escrow_refusal),
        Some("core.HoldableResource.open_holds_within_quantity")
    );

    // Releasing hold-1 frees its 60 units so hold-2 admits; reopening hold-1 is refused by core.Hold lifecycle.
    let h1_ver = cell.load(&hold1).map_err(|e| format!("{e:?}"))?.version;
    cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.EscrowHold",
                "hold-1",
                h1_ver,
                json!({"status":"released","quantity":60,"unit":"g","currency":null,"expires_at":null}),
            )?,
            put_write(
                "test.EscrowHold",
                "hold-2",
                FactVersion::ABSENT,
                json!({"status":"open","quantity":60,"unit":"g","currency":null,"expires_at":null}),
            )?,
        ],
        events: vec![transitioned(&hold1, "release", "open", "released")],
        links: vec![add_link(&hold2, "resource", &res)],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("releasing hold-1 frees capacity for hold-2: {err:?}"))?;

    let h1_released = cell.load(&hold1).map_err(|e| format!("{e:?}"))?.version;
    let reopen_refusal = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.EscrowHold",
            "hold-1",
            h1_released,
            json!({"status":"open","quantity":60,"unit":"g","currency":null,"expires_at":null}),
        )?],
        events: vec![transitioned(&hold1, "release", "released", "open")],
        ..CellCommit::empty()
    });
    assert!(
        reopen_refusal.is_err(),
        "reopening terminal released hold must be refused by inherited core.Hold lifecycle"
    );
    Ok(())
}

#[test]
fn core_physics_valued_event_balance_and_currency_enforce_at_commit() -> TestResult {
    let mut cell = core_physics_cell()?;
    let res = FactKey::new("test.HoldableHolding", "h-1");
    cell.commit(CellCommit {
        writes: vec![put_write(
            "test.HoldableHolding",
            "h-1",
            FactVersion::ABSENT,
            json!({"resource_code":"GOLD-1","quantity":100,"unit":"g","currency":null,"valuation_method":null}),
        )?],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("seed resource: {e:?}"))?;

    // Unbalanced postings (+100 / -90) are refused by core.ValuedEvent.effects_balance.
    let ev_unbal = FactKey::new("test.ValuedDelivery", "ev-unbal");
    let dr_u = FactKey::new("test.ValuedPosting", "p-dr-u");
    let cr_u = FactKey::new("test.ValuedPosting", "p-cr-u");
    let unbal = cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.ValuedDelivery",
                "ev-unbal",
                FactVersion::ABSENT,
                json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:00:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":false}),
            )?,
            put_write("test.ValuedPosting", "p-dr-u", FactVersion::ABSENT, json!({"account_code":"1100","amount":100,"currency":"USD"}))?,
            put_write("test.ValuedPosting", "p-cr-u", FactVersion::ABSENT, json!({"account_code":"4000","amount":-90,"currency":"USD"}))?,
        ],
        links: vec![
            add_link(&ev_unbal, "resource", &res),
            add_link(&dr_u, "event", &ev_unbal),
            add_link(&cr_u, "event", &ev_unbal),
        ],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&unbal),
        Some("core.ValuedEvent.effects_balance")
    );

    // Mixed currency (+100 EGP / -100 USD) is refused by core.ValuedLedgerEffect.currency_matches_event.
    let ev_mix = FactKey::new("test.ValuedDelivery", "ev-mix");
    let dr_m = FactKey::new("test.ValuedPosting", "p-dr-m");
    let cr_m = FactKey::new("test.ValuedPosting", "p-cr-m");
    let mix = cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.ValuedDelivery",
                "ev-mix",
                FactVersion::ABSENT,
                json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:00:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":false}),
            )?,
            put_write("test.ValuedPosting", "p-dr-m", FactVersion::ABSENT, json!({"account_code":"1100","amount":100,"currency":"EGP"}))?,
            put_write("test.ValuedPosting", "p-cr-m", FactVersion::ABSENT, json!({"account_code":"4000","amount":-100,"currency":"USD"}))?,
        ],
        links: vec![
            add_link(&ev_mix, "resource", &res),
            add_link(&dr_m, "event", &ev_mix),
            add_link(&cr_m, "event", &ev_mix),
        ],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&mix),
        Some("core.ValuedLedgerEffect.currency_matches_event")
    );

    // Balanced same-currency (+100 USD / -100 USD) admits cleanly.
    let ev_ok = FactKey::new("test.ValuedDelivery", "ev-ok");
    let dr_ok = FactKey::new("test.ValuedPosting", "p-dr-ok");
    let cr_ok = FactKey::new("test.ValuedPosting", "p-cr-ok");
    cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.ValuedDelivery",
                "ev-ok",
                FactVersion::ABSENT,
                json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:00:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":false}),
            )?,
            put_write("test.ValuedPosting", "p-dr-ok", FactVersion::ABSENT, json!({"account_code":"1100","amount":100,"currency":"USD"}))?,
            put_write("test.ValuedPosting", "p-cr-ok", FactVersion::ABSENT, json!({"account_code":"4000","amount":-100,"currency":"USD"}))?,
        ],
        links: vec![
            add_link(&ev_ok, "resource", &res),
            add_link(&dr_ok, "event", &ev_ok),
            add_link(&cr_ok, "event", &ev_ok),
        ],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("balanced same-currency posting must admit: {e:?}"))?;
    Ok(())
}

#[test]
fn core_physics_reversal_measure_and_commitment_lifecycle_enforce_at_commit() -> TestResult {
    let mut cell = core_physics_cell()?;
    let res = FactKey::new("test.HoldableHolding", "h-1");
    let orig = FactKey::new("test.ValuedDelivery", "ev-orig");
    cell.commit(CellCommit {
        writes: vec![
            put_write(
                "test.HoldableHolding",
                "h-1",
                FactVersion::ABSENT,
                json!({"resource_code":"GOLD-1","quantity":100,"unit":"g","currency":null,"valuation_method":null}),
            )?,
            put_write(
                "test.ValuedDelivery",
                "ev-orig",
                FactVersion::ABSENT,
                json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:00:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":false}),
            )?,
        ],
        links: vec![add_link(&orig, "resource", &res)],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("seed original event: {e:?}"))?;

    // Reversal flag without reverses link is refused.
    let unlinked_rev = FactKey::new("test.ValuedDelivery", "ev-unlinked-rev");
    let flag_refusal = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.ValuedDelivery",
            "ev-unlinked-rev",
            FactVersion::ABSENT,
            json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:01:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":true}),
        )?],
        links: vec![add_link(&unlinked_rev, "resource", &res)],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&flag_refusal),
        Some("core.ReversibleEvent.reversal_flag_requires_link")
    );

    // Reversal with mismatched quantity is refused.
    let wrong_qty = FactKey::new("test.ValuedDelivery", "ev-wrong-qty");
    let qty_refusal = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.ValuedDelivery",
            "ev-wrong-qty",
            FactVersion::ABSENT,
            json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:02:00Z","quantity":7,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":true}),
        )?],
        links: vec![
            add_link(&wrong_qty, "resource", &res),
            add_link(&wrong_qty, "reverses", &orig),
        ],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&qty_refusal),
        Some("core.EconomicEvent.reversal_matches_original")
    );

    // Exact reversal admits; reversing that reversal is refused.
    let rev = FactKey::new("test.ValuedDelivery", "ev-rev");
    cell.commit(CellCommit {
        writes: vec![put_write(
            "test.ValuedDelivery",
            "ev-rev",
            FactVersion::ABSENT,
            json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:03:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":true}),
        )?],
        links: vec![
            add_link(&rev, "resource", &res),
            add_link(&rev, "reverses", &orig),
        ],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("valid reversal must admit: {e:?}"))?;

    let rev_of_rev = FactKey::new("test.ValuedDelivery", "ev-rev-rev");
    let double_rev = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.ValuedDelivery",
            "ev-rev-rev",
            FactVersion::ABSENT,
            json!({"action":"transfer_ownership","occurred_at":"2026-11-27T12:04:00Z","quantity":10,"unit":"g","currency":null,"valuation_currency":"USD","is_reversal":true}),
        )?],
        links: vec![
            add_link(&rev_of_rev, "resource", &res),
            add_link(&rev_of_rev, "reverses", &rev),
        ],
        ..CellCommit::empty()
    });
    assert_eq!(
        violated_law_id(&double_rev),
        Some("core.ReversibleEvent.no_reversal_of_reversal")
    );
    Ok(())
}

#[test]
fn core_physics_commitment_lifecycle_enforces_at_commit() -> TestResult {
    let mut cell = core_physics_cell()?;
    let ol = FactKey::new("test.OrderLine", "ol-life");
    cell.commit(CellCommit {
        writes: vec![put_write(
            "test.OrderLine",
            "ol-life",
            FactVersion::ABSENT,
            json!({"status":"requested","due_at":"2026-12-01T00:00:00Z","quantity":5,"unit":"g","currency":null}),
        )?],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("requested commitment must admit: {e:?}"))?;
    let v1 = cell.load(&ol).map_err(|e| format!("{e:?}"))?.version;
    cell.commit(CellCommit {
        writes: vec![put_write(
            "test.OrderLine",
            "ol-life",
            v1,
            json!({"status":"promised","due_at":"2026-12-01T00:00:00Z","quantity":5,"unit":"g","currency":null}),
        )?],
        events: vec![transitioned(&ol, "promise", "requested", "promised")],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("requested -> promised must admit: {e:?}"))?;
    let v2 = cell.load(&ol).map_err(|e| format!("{e:?}"))?.version;
    cell.commit(CellCommit {
        writes: vec![put_write(
            "test.OrderLine",
            "ol-life",
            v2,
            json!({"status":"fulfilled","due_at":"2026-12-01T00:00:00Z","quantity":5,"unit":"g","currency":null}),
        )?],
        events: vec![transitioned(&ol, "fulfil", "promised", "fulfilled")],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("promised -> fulfilled must admit: {e:?}"))?;
    let v3 = cell.load(&ol).map_err(|e| format!("{e:?}"))?.version;
    let reopen = cell.commit(CellCommit {
        writes: vec![put_write(
            "test.OrderLine",
            "ol-life",
            v3,
            json!({"status":"promised","due_at":"2026-12-01T00:00:00Z","quantity":5,"unit":"g","currency":null}),
        )?],
        events: vec![transitioned(&ol, "promise", "fulfilled", "promised")],
        ..CellCommit::empty()
    });
    assert!(
        reopen.is_err(),
        "reopening terminal fulfilled commitment must be refused by inherited core.Commitment lifecycle"
    );
    Ok(())
}
