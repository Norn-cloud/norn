#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test fixtures"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! Shared commit admission keeps declared accounting legs balanced and immutable.

use norn_ir::ObjectTypeDef;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Effect, FactKey, FactMutation, FactVersion,
    FactWrite, FreshnessPolicy, Generation, IdempotencyKey, TypeCatalog,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<String, JsonObject, JsonObject>;

const LEG_TYPE: &str = "ledger.GLLeg";
const BASE_TYPE: &str = "ledger.BaseLeg";

fn types() -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": BASE_TYPE,
            "properties": [
                { "id": "journal_id", "type": { "kind": "text" } },
                { "id": "leg", "type": { "kind": "json-ext" } }
            ]
        }))
        .expect("unmarked base leg type decodes"),
        serde_json::from_value(json!({
            "name": LEG_TYPE,
            "extends": BASE_TYPE,
            "accountingLeg": {
                "journalIdProperty": "journal_id",
                "legProperty": "leg"
            }
        }))
        .expect("base leg type decodes"),
        serde_json::from_value(json!({
            "name": "ledger.SpecialLeg",
            "extends": LEG_TYPE
        }))
        .expect("subtype decodes"),
    ]
}

fn write(key: &str, book: &str, unit: &str, amount: &str, journal: &str) -> FactWrite {
    let record: JsonObject = serde_json::from_value(json!({
        "journal_id": journal,
        "leg": {
            "legId": key,
            "account": {
                "scopeKey": "tenant-1",
                "bookId": book,
                "graphId": "chart-1",
                "graphVersion": 4,
                "accountId": format!("account-{key}")
            },
            "unitId": unit,
            "unitPrecision": 2,
            "layerId": "actual",
            "amountUnscaled": amount
        }
    }))
    .expect("runtime leg values decode");
    FactWrite {
        key: FactKey::new(LEG_TYPE, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put { values: record },
    }
}

fn subtype_write(key: &str, amount: &str) -> FactWrite {
    let mut write = write(key, "book-1", "USD", amount, "journal-1");
    write.key.object_type = norn_kernel::cell_host::ObjectTypeName("ledger.SpecialLeg".to_owned());
    write
}

fn cell() -> Cell {
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(types())
}

fn commit_balanced_legs(cell: &mut Cell) {
    cell.commit(CellCommit {
        writes: vec![
            write("debit", "book-1", "USD", "100", "journal-immutable"),
            write("credit", "book-1", "USD", "-100", "journal-immutable"),
        ],
        ..CellCommit::empty()
    })
    .expect("balanced posting commits");
}

fn perform_effect() -> Effect<JsonObject, JsonObject> {
    Effect::Perform(Command {
        id: CommandId("side-effect".to_owned()),
        generation: Generation(0),
        idempotency_key: IdempotencyKey("side-effect".to_owned()),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload: JsonObject::new(),
    })
}

#[test]
fn balanced_exact_posting_group_commits_including_inherited_marker() {
    let mut cell = cell();
    let commit = CellCommit {
        writes: vec![
            subtype_write("leg-debit", "9007199254740993"),
            write(
                "leg-credit",
                "book-1",
                "USD",
                "-9007199254740993",
                "journal-1",
            ),
        ],
        ..CellCommit::empty()
    };

    cell.commit(commit).expect("exactly balanced group commits");
    assert!(
        cell.load(&FactKey::new(LEG_TYPE, "leg-debit"))
            .unwrap()
            .exists()
    );
    assert_eq!(cell.ledger().len(), 1);
}

#[test]
fn unbalanced_group_refusal_has_no_fact_ledger_or_outbox_effects() {
    let mut cell = cell();
    let commit = CellCommit {
        writes: vec![
            write("leg-debit", "book-1", "USD", "1200", "journal-1"),
            write("leg-credit", "book-1", "USD", "-1199", "journal-1"),
        ],
        events: vec!["business event".to_owned()],
        effects: vec![perform_effect()],
        ..CellCommit::empty()
    };

    let refusal = cell.commit(commit).expect_err("unbalanced postings refuse");
    assert_eq!(refusal.code(), "journal.unbalanced");
    assert!(
        !cell
            .load(&FactKey::new(LEG_TYPE, "leg-debit"))
            .unwrap()
            .exists()
    );
    assert!(cell.ledger().is_empty());
    assert!(cell.outbox().is_empty());
}

#[test]
fn multiple_currencies_balance_independently_and_cannot_cancel_across_units() {
    let mut balanced_cell = cell();
    let balanced = CellCommit {
        writes: vec![
            write("usd-debit", "book-1", "USD", "100", "journal-fx"),
            write("usd-credit", "book-1", "USD", "-100", "journal-fx"),
            write("eur-debit", "book-1", "EUR", "250", "journal-fx"),
            write("eur-credit", "book-1", "EUR", "-250", "journal-fx"),
        ],
        ..CellCommit::empty()
    };
    balanced_cell
        .commit(balanced)
        .expect("each currency balances");

    let mut unbalanced = cell();
    let cross_currency = CellCommit {
        writes: vec![
            write("usd-debit", "book-1", "USD", "100", "journal-fx"),
            write("eur-credit", "book-1", "EUR", "-100", "journal-fx"),
        ],
        ..CellCommit::empty()
    };
    let refusal = unbalanced
        .commit(cross_currency)
        .expect_err("a net-zero total across currencies is not balanced");
    assert_eq!(refusal.code(), "journal.unbalanced_by_unit");
    assert!(unbalanced.ledger().is_empty());
}

#[test]
fn a_journal_id_cannot_balance_across_books() {
    let mut cell = cell();
    let cross_book = CellCommit {
        writes: vec![
            write("debit", "book-1", "USD", "100", "journal-shared"),
            write("credit", "book-2", "USD", "-100", "journal-shared"),
        ],
        ..CellCommit::empty()
    };
    let refusal = cell
        .commit(cross_book)
        .expect_err("books are independent accounting groups");
    assert_eq!(refusal.code(), "journal.too_few_legs");
    assert!(cell.ledger().is_empty());
}

#[test]
fn committed_leg_records_are_create_only_and_cannot_be_deleted() {
    let mut cell = cell();
    commit_balanced_legs(&mut cell);

    let key = FactKey::new(LEG_TYPE, "debit");
    let values = cell.load(&key).unwrap().values.unwrap();
    let update = FactWrite {
        key: key.clone(),
        expected: FactVersion(1),
        mutation: FactMutation::Put { values },
    };
    assert_eq!(
        cell.commit(CellCommit {
            writes: vec![update],
            ..CellCommit::empty()
        })
        .unwrap_err()
        .code(),
        "journal.posted_immutable"
    );
    let delete = FactWrite {
        key,
        expected: FactVersion(1),
        mutation: FactMutation::Delete,
    };
    assert_eq!(
        cell.commit(CellCommit {
            writes: vec![delete],
            ..CellCommit::empty()
        })
        .unwrap_err()
        .code(),
        "journal.delete_forbidden"
    );
    assert_eq!(cell.ledger().len(), 1);
}

#[test]
fn deleting_a_posted_leg_through_its_unmarked_ancestor_is_refused_atomically() {
    let mut cell = cell();
    commit_balanced_legs(&mut cell);
    let key = FactKey::new(BASE_TYPE, "debit");
    let before = cell.load(&FactKey::new(LEG_TYPE, "debit")).unwrap();
    let ledger_len = cell.ledger().len();
    let outbox_len = cell.outbox().len();

    let refusal = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key,
                expected: FactVersion(1),
                mutation: FactMutation::Delete,
            }],
            effects: vec![perform_effect()],
            ..CellCommit::empty()
        })
        .expect_err("ancestor alias cannot delete a posted leg");

    assert_eq!(refusal.code(), "journal.delete_forbidden");
    assert_eq!(cell.load(&FactKey::new(LEG_TYPE, "debit")).unwrap(), before);
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(cell.outbox().len(), outbox_len);
}

#[test]
fn updating_a_posted_leg_through_its_unmarked_ancestor_is_refused_atomically() {
    let mut cell = cell();
    commit_balanced_legs(&mut cell);
    let live = cell.load(&FactKey::new(LEG_TYPE, "debit")).unwrap();
    let before = live.clone();
    let ledger_len = cell.ledger().len();
    let outbox_len = cell.outbox().len();

    let refusal = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(BASE_TYPE, "debit"),
                expected: live.version,
                mutation: FactMutation::Put {
                    values: live.values.expect("posted leg has values"),
                },
            }],
            effects: vec![perform_effect()],
            ..CellCommit::empty()
        })
        .expect_err("ancestor alias cannot update a posted leg");

    assert_eq!(refusal.code(), "journal.posted_immutable");
    assert_eq!(cell.load(&FactKey::new(LEG_TYPE, "debit")).unwrap(), before);
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(cell.outbox().len(), outbox_len);
}

#[test]
fn accounting_leg_declaration_is_resolved_from_the_type_catalog() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let model = norn_kernel::CommitModel::new(&catalog);
    assert!(model.accounting_leg(&types[1].name).unwrap().is_some());
}
