#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test fixtures"
)]

//! `norn.core` inheritance at runtime: a workflow built only from the posting
//! primitive keeps every committed journal balanced and every posted leg
//! unchanged, whatever its steps (`INV-CORE-VALUE-CONSERVATION`,
//! `INV-CORE-POSTED-IMMUTABLE`). This is the runtime face of `run_balanced`
//! and `run_append_only` in `verification/core-physics-v1/proofs/CoreValueLedger.lean`.
//!
//! Workflows are generated deterministically, and each step is checked
//! against the reference semantics: admit exactly the balanced posts, refuse
//! every edit or delete of a posted leg.

use std::collections::BTreeMap;

use norn_ir::ObjectTypeDef;
use norn_ir::posting::AccountingLegIr;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{CellCommit, CellHost, Command, FactKey, FactMutation, FactVersion, FactWrite};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<String, JsonObject, JsonObject>;

const LEG_TYPE: &str = "core.PostingLeg";
const UNITS: [&str; 2] = ["EGP", "USD"];

fn cell() -> Cell {
    let leg: ObjectTypeDef = serde_json::from_value(json!({
        "name": LEG_TYPE,
        "properties": [
            { "id": "journal_id", "type": { "kind": "text" } },
            { "id": "leg", "type": { "kind": "json-ext" } }
        ],
        "accountingLeg": { "journalIdProperty": "journal_id", "legProperty": "leg" }
    }))
    .expect("posting leg type decodes");
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(vec![leg])
}

/// Deterministic xorshift, so every run explores the same workflows.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0.wrapping_shl(13);
        self.0 ^= self.0.wrapping_shr(7);
        self.0 ^= self.0.wrapping_shl(17);
        usize::try_from(self.0.checked_rem(u64::try_from(bound).unwrap()).unwrap()).unwrap()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Leg {
    key: String,
    account: usize,
    unit: &'static str,
    amount: i64,
}

fn put(journal: &str, leg: &Leg) -> FactWrite {
    let values: JsonObject = serde_json::from_value(json!({
        "journal_id": journal,
        "leg": {
            "legId": leg.key,
            "account": {
                "scopeKey": "tenant-1",
                "bookId": "book-1",
                "graphId": "chart-1",
                "graphVersion": 1,
                "accountId": format!("account-{}", leg.account)
            },
            "unitId": leg.unit,
            "unitPrecision": 2,
            "layerId": "actual",
            "amountUnscaled": leg.amount.to_string()
        }
    }))
    .expect("leg values decode");
    FactWrite {
        key: FactKey::new(LEG_TYPE, &leg.key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put { values },
    }
}

/// The reference admission check: every unit nets to zero, with at least two legs.
fn balanced(legs: &[Leg]) -> bool {
    legs.len() >= 2
        && UNITS.iter().all(|unit| {
            legs.iter()
                .filter(|leg| leg.unit == *unit)
                .map(|leg| leg.amount)
                .sum::<i64>()
                == 0
        })
}

fn journal(rng: &mut Rng, step: usize) -> Vec<Leg> {
    let count = rng.below(3).checked_add(2).unwrap();
    let mut legs: Vec<Leg> = (0..count)
        .map(|i| Leg {
            key: format!("s{step}-l{i}"),
            account: rng.below(4),
            unit: UNITS[rng.below(2)],
            amount: i64::try_from(rng.below(9)).unwrap().checked_sub(4).unwrap(),
        })
        .collect();
    // Half of the generated journals are closed into balance by their last leg.
    if rng.below(2) == 0 {
        let last = legs.len().checked_sub(1).unwrap();
        legs[last].unit = legs[0].unit;
        let rest: i64 = legs[..last]
            .iter()
            .filter(|leg| leg.unit == legs[0].unit)
            .map(|leg| leg.amount)
            .sum();
        legs[last].amount = rest.checked_neg().unwrap();
    }
    legs
}

fn trial_balance(cell: &Cell, posted: &BTreeMap<String, Leg>) -> BTreeMap<String, i64> {
    let mut totals = BTreeMap::new();
    for leg in posted.values() {
        let state = cell.load(&FactKey::new(LEG_TYPE, &leg.key)).unwrap();
        let values = state.values.expect("posted leg is live");
        let stored: AccountingLegIr =
            norn_ir::runtime::decode(values.get("leg").expect("leg value")).expect("leg decodes");
        assert_eq!(
            stored.unit_id, leg.unit,
            "stored leg {} changed unit",
            leg.key
        );
        let amount: i64 = stored.amount_unscaled.parse().unwrap();
        let total = totals.entry(stored.unit_id).or_insert(0_i64);
        *total = total.checked_add(amount).unwrap();
    }
    totals
}

#[test]
fn every_generated_workflow_of_primitive_steps_inherits_conservation_and_immutability() {
    let mut admitted = 0_u32;
    let mut refused = 0_u32;
    for seed in 1..=150_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut cell = cell();
        let mut posted: BTreeMap<String, Leg> = BTreeMap::new();
        for step in 0..8 {
            let before: Vec<_> = posted
                .keys()
                .map(|key| {
                    (
                        key.clone(),
                        cell.load(&FactKey::new(LEG_TYPE, key)).unwrap(),
                    )
                })
                .collect();
            let choice = rng.below(4);
            if choice < 2 || posted.is_empty() {
                let legs = journal(&mut rng, step);
                let writes = legs
                    .iter()
                    .map(|leg| put(&format!("j{step}"), leg))
                    .collect();
                let result = cell.commit(CellCommit {
                    writes,
                    ..CellCommit::empty()
                });
                assert_eq!(
                    result.is_ok(),
                    balanced(&legs),
                    "seed {seed} step {step}: admission must match the reference check"
                );
                if result.is_ok() {
                    admitted = admitted.checked_add(1).unwrap();
                    posted.extend(legs.into_iter().map(|leg| (leg.key.clone(), leg)));
                } else {
                    for leg in &legs {
                        assert!(
                            !cell
                                .load(&FactKey::new(LEG_TYPE, &leg.key))
                                .unwrap()
                                .exists(),
                            "seed {seed} step {step}: a refused journal left leg {}",
                            leg.key
                        );
                    }
                    refused = refused.checked_add(1).unwrap();
                }
            } else {
                let keys: Vec<_> = posted.keys().cloned().collect();
                let key = FactKey::new(LEG_TYPE, &keys[rng.below(keys.len())]);
                let live = cell.load(&key).unwrap();
                let mutation = if choice == 2 {
                    FactMutation::Put {
                        values: live.values.clone().unwrap(),
                    }
                } else {
                    FactMutation::Delete
                };
                let write = FactWrite {
                    key,
                    expected: live.version,
                    mutation,
                };
                assert!(
                    cell.commit(CellCommit {
                        writes: vec![write],
                        ..CellCommit::empty()
                    })
                    .is_err(),
                    "seed {seed} step {step}: an edit or delete of a posted leg must be refused"
                );
                refused = refused.checked_add(1).unwrap();
            }
            for (key, state) in &before {
                assert_eq!(
                    &cell.load(&FactKey::new(LEG_TYPE, key)).unwrap(),
                    state,
                    "seed {seed} step {step}: posted leg {key} changed"
                );
            }
            assert!(
                trial_balance(&cell, &posted)
                    .values()
                    .all(|total| *total == 0),
                "seed {seed} step {step}: the trial balance must stay zero in every unit"
            );
        }
    }
    // The generator must exercise both outcomes, or the test proves nothing.
    assert!(
        admitted > 100 && refused > 100,
        "admitted {admitted}, refused {refused}"
    );
}
