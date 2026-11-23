#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    missing_docs,
    reason = "kernel commit-time invariant enforcement over registry-typed properties"
)]

//! Verifies that `norn-kernel` enforces interface invariants reading
//! registry-typed (`Norn.ref`) properties at commit time (#1583).

use norn_ir::{InterfaceDef, ObjectTypeDef, PropertyTypeDef};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{CellCommit, CellHost, FactEvent, FactKey, FactMutation, FactVersion, FactWrite};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn core_property_types() -> Vec<PropertyTypeDef> {
    [
        include_str!("../../../registry/core/artifacts/core.value_types.entity.json"),
        include_str!("../../../registry/core/artifacts/std.value_types.entity.json"),
    ]
    .into_iter()
    .flat_map(|raw| {
        let artifact: Value = serde_json::from_str(raw).unwrap();
        let types = artifact
            .get("payload")
            .and_then(|payload| payload.get("propertyTypes"))
            .expect("propertyTypes")
            .clone();
        serde_json::from_value::<Vec<PropertyTypeDef>>(types).unwrap()
    })
    .collect()
}

fn core_interfaces() -> Vec<InterfaceDef> {
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
        let artifact: Value = serde_json::from_str(raw).unwrap();
        let interface = artifact
            .get("payload")
            .and_then(|payload| payload.get("interface"))
            .expect("interface")
            .clone();
        serde_json::from_value::<InterfaceDef>(interface).unwrap()
    })
    .collect()
}

fn rea_object_types() -> Vec<ObjectTypeDef> {
    vec![
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
        }))
        .unwrap(),
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
        }))
        .unwrap(),
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
        }))
        .unwrap(),
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
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": "test.Posting",
            "implements": ["core.LedgerEffect"],
            "properties": [
                { "id": "account_code", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "amount", "type": { "kind": "ref", "name": "core.money_amount" } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" } }
            ]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": "test.Receivable",
            "implements": ["core.Claim"],
            "properties": [
                { "id": "amount", "type": { "kind": "ref", "name": "core.money_amount" } },
                { "id": "currency", "type": { "kind": "ref", "name": "core.code" } },
                { "id": "due_date", "type": { "kind": "ref", "name": "iso8601.date" } }
            ]
        }))
        .unwrap(),
    ]
}

fn rea_cell() -> Cell {
    Cell::new(|command| command.payload.clone())
        .with_types(rea_object_types())
        .with_interfaces(core_interfaces())
        .with_property_types(core_property_types())
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
fn core_rea_invariants_admit_valid_records_at_commit() {
    let mut cell = rea_cell();

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
}

#[test]
fn core_rea_invariants_refuse_violations_at_commit() {
    let mut cell = rea_cell();

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
        let err = put(&mut cell, object_type, key, values)
            .expect_err(&format!("{object_type}/{key} must refuse"));
        assert_eq!(
            err, "commit.invariant_violation",
            "{object_type}/{key} expected commit.invariant_violation"
        );
        assert!(
            !cell.load(&FactKey::new(object_type, key)).unwrap().exists(),
            "{object_type}/{key} must not persist after refusal"
        );
    }
}
