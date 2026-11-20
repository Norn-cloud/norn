//! A host's own provisioning writes: fixed inputs only (#1375 S4, #1444 fix 5).
//!
//! The raw store commit runs no law check, so it is private to this crate:
//! an authored write reaches storage only through the kernel path
//! ([`crate::host`]). A host still seeds two fixed, synthetic row sets
//! without an actor: the public demo cell's catalog rows ([`demo_rows`]) and
//! the analytics cell's recorded K1 conversions ([`k1_conversions`]). Their
//! rows are compiled into this crate, so a caller chooses only the cell and
//! its scope, never what is written:
//!
//! ```compile_fail
//! // There is no operation that takes caller-chosen rows.
//! use norn_cell::{CellStore, CommitBatch};
//! fn bypass(store: &mut CellStore) {
//!     let _ = norn_cell::provision::demo_rows(store, "scope", &CommitBatch::default());
//! }
//! ```
//!
//! ```compile_fail
//! // The former trusted door is gone.
//! use norn_cell::trusted::TrustedCommit;
//! ```

use std::collections::BTreeMap;

use norn_ir::{Decimal, RuntimeValue};

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::store::{CellStore, CommitBatch, CommitOutcome, FactWriteRow, MAIN_WORLD};

/// The idempotency key of the demo rows' one commit: a restarted host
/// replays it without writing, and nothing is ever wiped.
pub const DEMO_ROWS_KEY: &str = "demo.seed.rows/1";

/// Logical time of the demo rows' commit; fixed so every boot is identical.
pub const DEMO_ROWS_AT: u64 = 2;

/// Synthetic catalog items (no real or shop data): id, code and label.
pub const DEMO_ITEMS: [(&str, &str, &str); 6] = [
    ("SYN-ITEM-01", "BAR-1KG", "Synthetic gold bar, 1 kg"),
    ("SYN-ITEM-02", "BAR-100G", "Synthetic gold bar, 100 g"),
    ("SYN-ITEM-03", "COIN-1OZ", "Synthetic silver coin, 1 oz"),
    ("SYN-ITEM-04", "GRAIN-AU", "Synthetic gold grain"),
    ("SYN-ITEM-05", "SCRAP-AG", "Synthetic silver scrap"),
    ("SYN-ITEM-06", "WIRE-PT", "Synthetic platinum wire"),
];

/// Synthetic parties: id, display name and kind.
pub const DEMO_PARTIES: [(&str, &str, &str); 4] = [
    ("SYN-PARTY-01", "Synthetic Refinery A", "organization"),
    ("SYN-PARTY-02", "Synthetic Vault B", "organization"),
    ("SYN-PARTY-03", "Synthetic Assayer", "person"),
    ("SYN-PARTY-04", "Synthetic Ledger Feed", "system"),
];

/// The analytics conversion type the K1 rows are written as.
pub const K1_TYPE: &str = "demo.Conversion";
/// The world the K1 rows are written in.
pub const K1_WORLD: &str = "synthetic-k1";
/// The recorded K1 observation clock (2026-10-23T00:00:00Z).
pub const K1_CLOCK: u64 = 1_792_972_800_000;

const K1_CSV: &str = include_str!("../../../fixtures/demo/scrap-yield-k1.csv");

/// Commit the demo rows ([`DEMO_ITEMS`], [`DEMO_PARTIES`]) into the main
/// world once. A cell that already holds them writes nothing; a cell that
/// committed past [`DEMO_ROWS_AT`] before it was seeded takes the rows at its
/// clock floor, never behind it.
///
/// # Errors
///
/// Any [`CellError`]; on error nothing was written.
pub fn demo_rows<E: SqlEngine>(store: &mut CellStore<E>, scope: &str) -> Result<(), CellError> {
    let floor = {
        let reader = store.read();
        if reader
            .receipt_by_idempotency_key(MAIN_WORLD, DEMO_ROWS_KEY)?
            .is_some()
        {
            return Ok(());
        }
        reader.clock_floor()?
    };
    let text = |value: &str| RuntimeValue::from(value);
    let items = DEMO_ITEMS
        .iter()
        .map(|(id, code, label)| FactWriteRow::Put {
            object_type: "canon.Item".into(),
            key: (*id).into(),
            values: BTreeMap::from([
                ("id".to_owned(), text(id)),
                ("code".to_owned(), text(code)),
                ("label".to_owned(), text(label)),
            ]),
        });
    let parties = DEMO_PARTIES
        .iter()
        .map(|(id, name, kind)| FactWriteRow::Put {
            object_type: "canon.Party".into(),
            key: (*id).into(),
            values: BTreeMap::from([
                ("display_name".to_owned(), text(name)),
                ("kind".to_owned(), text(kind)),
            ]),
        });
    store
        .commit_unchecked(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: scope.to_owned(),
            logical_time: DEMO_ROWS_AT.max(floor),
            idempotency_key: Some(DEMO_ROWS_KEY.into()),
            writes: items.chain(parties).collect(),
            ..CommitBatch::default()
        })
        .map(|_outcome| ())
}

/// Commit the recorded K1 conversions (`fixtures/demo/scrap-yield-k1.csv`)
/// into [`K1_WORLD`] at [`K1_CLOCK`], as one commit. The host calls it once,
/// when the cell holds no K1 load yet.
///
/// # Errors
///
/// A malformed fixture row, or any [`CellError`]; on error nothing was
/// written.
pub fn k1_conversions<E: SqlEngine>(
    store: &mut CellStore<E>,
    scope: &str,
) -> Result<CommitOutcome, CellError> {
    store.commit_unchecked(&CommitBatch {
        world: K1_WORLD.into(),
        scope: scope.to_owned(),
        logical_time: K1_CLOCK,
        writes: k1_rows()?,
        ..CommitBatch::default()
    })
}

/// The K1 conversion rows, as [`k1_conversions`] writes them.
///
/// # Errors
///
/// A malformed fixture row.
pub fn k1_rows() -> Result<Vec<FactWriteRow>, CellError> {
    let grams = |mg: &str| -> Result<RuntimeValue, CellError> {
        Ok(RuntimeValue::from(Decimal::parse(&format!("{mg}e-3"))?))
    };
    K1_CSV
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            let &[_, _, key, metal, input, recovered, loss, _, at, _] = fields.as_slice() else {
                return Err(CellError::Encoding(format!(
                    "malformed conversion row: {line}"
                )));
            };
            Ok(FactWriteRow::Put {
                object_type: K1_TYPE.into(),
                key: key.into(),
                values: BTreeMap::from([
                    ("input".into(), grams(input)?),
                    ("recovered".into(), grams(recovered)?),
                    ("loss".into(), grams(loss)?),
                    ("occurred_at".into(), RuntimeValue::from(at)),
                    ("metal".into(), RuntimeValue::from(metal)),
                ]),
            })
        })
        .collect()
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::expect_used, reason = "provisioning fixture assertions")]
mod tests {
    use super::{
        DEMO_ITEMS, DEMO_PARTIES, DEMO_ROWS_KEY, K1_TYPE, demo_rows, k1_conversions, k1_rows,
    };
    use crate::{CellStore, MAIN_WORLD, Model};
    use norn_ir::ObjectTypeDef;
    use serde_json::json;

    fn object(name: &str, properties: &[(&str, serde_json::Value)]) -> ObjectTypeDef {
        let properties: Vec<_> = properties
            .iter()
            .map(|(id, kind)| json!({"id": id, "type": kind}))
            .collect();
        serde_json::from_value(json!({"name": name, "properties": properties})).expect("type")
    }

    fn demo_store() -> CellStore {
        let text = json!({"kind": "text"});
        let model = Model::from_definitions(
            &[
                object(
                    "canon.Item",
                    &[
                        ("id", text.clone()),
                        ("code", text.clone()),
                        ("label", text.clone()),
                    ],
                ),
                object(
                    "canon.Party",
                    &[("display_name", text.clone()), ("kind", text)],
                ),
            ],
            &[],
        )
        .expect("model");
        CellStore::open_in_memory(model).expect("store")
    }

    /// The demo op writes exactly the compiled-in rows, once.
    #[test]
    fn demo_rows_write_the_fixed_rows_once() {
        let mut store = demo_store();
        demo_rows(&mut store, "org_demo/dev").expect("seed");
        let after_first = store.read().dump().expect("dump");
        let head = store.verified_head().expect("head").commits;
        for (id, _, _) in DEMO_ITEMS {
            let row = store
                .read()
                .object(MAIN_WORLD, "canon.Item", id)
                .expect("read")
                .expect("seeded item");
            assert_eq!(row.scope, "org_demo/dev");
        }
        for (id, _, _) in DEMO_PARTIES {
            assert!(
                store
                    .read()
                    .object(MAIN_WORLD, "canon.Party", id)
                    .expect("read")
                    .is_some()
            );
        }
        let receipt = store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, DEMO_ROWS_KEY)
            .expect("receipt")
            .expect("one seed receipt");
        assert_eq!(
            receipt.body.written.len(),
            DEMO_ITEMS.len() + DEMO_PARTIES.len()
        );
        demo_rows(&mut store, "org_demo/dev").expect("replay");
        assert_eq!(
            store.verified_head().expect("head").commits,
            head,
            "no second commit"
        );
        assert_eq!(
            store.read().dump().expect("dump"),
            after_first,
            "nothing written"
        );
    }

    /// The K1 op writes exactly the recorded fixture rows in the K1 world.
    #[test]
    fn k1_conversions_write_the_recorded_rows() {
        let mass = json!({"kind":"decimal","additivity":{"kind":"additive"},"scale":3,"unit":{"system":"ucum","code":"g"}});
        let model = Model::from_definitions(
            &[object(
                K1_TYPE,
                &[
                    ("input", mass.clone()),
                    ("recovered", mass.clone()),
                    ("loss", mass),
                    ("occurred_at", json!({"kind":"timestamp","timezone":"utc"})),
                    ("metal", json!({"kind":"text"})),
                ],
            )],
            &[],
        )
        .expect("model");
        let mut store = CellStore::open_in_memory(model).expect("store");
        let rows = k1_rows().expect("fixture rows");
        assert!(!rows.is_empty());
        let outcome = k1_conversions(&mut store, "org_k1/dev").expect("K1");
        assert_eq!(outcome.receipt().body.written.len(), rows.len());
        assert!(
            outcome
                .receipt()
                .body
                .written
                .iter()
                .all(|row| row.object_type == K1_TYPE)
        );
        assert_eq!(store.verified_head().expect("head").commits, 1);
    }
}
