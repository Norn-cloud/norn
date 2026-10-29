#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "quantity refusal boundary regression assertions"
)]
//! Dependency discovery and admission must agree before selecting a remainder.
mod quantity_support;

use norn_cell::{CellError, CellStore, CommitBatch, CommitOutcome, Model, StoreOptions};
use norn_ir::quantity_ledger::{
    LedgerFundingPolicy, QuantityBalanceVersion, QuantityOperation, QuantityRequest,
    QuantityResolution, QuantitySlice,
};
use norn_kernel::quantity::{
    QuantityError, QuantityState, prepare_quantity, quantity_balance_keys,
};
use quantity_support::{define, post, request, reserve};

fn model() -> Model {
    Model::from_definitions(&[], &[]).expect("model")
}

fn commit(
    store: &mut CellStore,
    quantity: &QuantityRequest,
    key: &str,
    now: u64,
) -> Result<CommitOutcome, CellError> {
    store.commit(&CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        logical_time: now,
        idempotency_key: Some(key.into()),
        quantity: Some(quantity.clone()),
        ..CommitBatch::default()
    })
}

fn fenced(store: &mut CellStore, quantity: &QuantityRequest, key: &str, now: u64) -> CommitOutcome {
    let mut quantity = quantity.clone();
    quantity.balance_versions = store
        .read()
        .quantity_balance_versions("main", &quantity)
        .expect("verified dependency discovery");
    commit(store, &quantity, key, now).expect("fixture commit")
}

fn initialized() -> CellStore {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    fenced(&mut store, &define(), "define", 0);
    fenced(&mut store, &post(0, "fund", "fund", 10), "fund", 0);
    store
}

fn subset(q: i128) -> QuantityResolution {
    QuantityResolution::Subset {
        legs: [("out", q.checked_neg().expect("bounded slice")), ("in", q)]
            .into_iter()
            .map(|(id, amount)| QuantitySlice {
                leg_id: id.into(),
                amount_unscaled: amount.to_string(),
            })
            .collect(),
    }
}

fn resolution_operation(kind: &str, resolution: QuantityResolution) -> QuantityOperation {
    match kind {
        "post" => QuantityOperation::PostReservation {
            reservation_id: "r".into(),
            posting_id: "p".into(),
            resolution,
        },
        "void" => QuantityOperation::Void {
            reservation_id: "r".into(),
            resolution,
        },
        "expire" => QuantityOperation::Expire {
            reservation_id: "r".into(),
        },
        _ => unreachable!("closed test operation"),
    }
}

fn pure_state(store: &CellStore, quantity: &QuantityRequest) -> QuantityState {
    let reservation_id = match &quantity.operation {
        QuantityOperation::PostReservation { reservation_id, .. }
        | QuantityOperation::Void { reservation_id, .. }
        | QuantityOperation::Expire { reservation_id } => reservation_id,
        QuantityOperation::Define { .. }
        | QuantityOperation::Post { .. }
        | QuantityOperation::Reserve { .. }
        | QuantityOperation::Reverse { .. } => unreachable!("resolution request"),
    };
    QuantityState {
        version: store
            .read()
            .quantity_version("main", "stock")
            .expect("version"),
        pack: Some(quantity_support::pack()),
        reservation: store
            .read()
            .quantity_reservation("main", "stock", reservation_id)
            .expect("reservation"),
        identity_exists: if let QuantityOperation::PostReservation { posting_id, .. } =
            &quantity.operation
        {
            store
                .read()
                .quantity_posting("main", "stock", posting_id)
                .expect("posting")
                .is_some()
        } else {
            false
        },
        ..QuantityState::default()
    }
}

fn assert_refusal(
    store: &mut CellStore,
    quantity: &QuantityRequest,
    now: u64,
    expected: &QuantityError,
) {
    let state = pure_state(store, quantity);
    assert_eq!(
        quantity_balance_keys(quantity, &state),
        Err(expected.clone())
    );
    assert_eq!(
        prepare_quantity(quantity, &state, now).expect_err("pure refusal"),
        *expected
    );
    let before = store.read().dump().expect("before");
    assert!(
        matches!(store.read().quantity_balance_versions("main", quantity),
        Err(CellError::Quantity(error)) if &error == expected)
    );
    let result = commit(store, quantity, "fresh-refusal", now);
    assert!(
        matches!(&result, Err(CellError::Quantity(error)) if error == expected),
        "expected {expected:?}, got {result:?}"
    );
    assert_eq!(before, store.read().dump().expect("unchanged"));
}

#[test]
fn missing_and_terminal_resolution_matrix_matches_discovery_and_admission() {
    for status in ["missing", "posted", "void", "expired"] {
        let mut store = initialized();
        if status != "missing" {
            fenced(&mut store, &reserve(0, "r", 2, 3), "reserve", 0);
            let kind = if status == "posted" {
                "post"
            } else if status == "expired" {
                "expire"
            } else {
                "void"
            };
            let mut terminal = request(0, resolution_operation(kind, QuantityResolution::All));
            if let QuantityOperation::PostReservation { posting_id, .. } = &mut terminal.operation {
                *posting_id = "terminal-post".into();
            }
            fenced(
                &mut store,
                &terminal,
                "resolve",
                if status == "expired" { 4 } else { 0 },
            );
        }
        for kind in ["post", "void", "expire"] {
            // Expire has no selection field: its only contract is the full remainder.
            let selections = if kind == "expire" {
                vec![QuantityResolution::All]
            } else {
                vec![QuantityResolution::All, subset(1), subset(3), subset(0)]
            };
            for selection in selections {
                let quantity = request(0, resolution_operation(kind, selection));
                assert_refusal(&mut store, &quantity, 4, &QuantityError::Terminal);
                let mut invalid = quantity.clone();
                invalid.version = "unsupported".into();
                assert_refusal(&mut store, &invalid, 4, &QuantityError::InvalidContract);
                invalid = quantity.clone();
                invalid.ledger_id = " stock".into();
                // An invalid envelope wins even when the named ledger/reservation is absent.
                assert_eq!(
                    quantity_balance_keys(&invalid, &pure_state(&store, &quantity)),
                    Err(QuantityError::InvalidContract)
                );
                assert!(matches!(
                    commit(&mut store, &invalid, "invalid-ledger", 4),
                    Err(CellError::Quantity(QuantityError::InvalidContract))
                ));
            }
        }
    }
}

#[test]
fn terminal_retries_duplicates_clock_and_live_invalid_subsets_keep_precedence() {
    for kind in ["post", "void", "expire"] {
        let mut store = initialized();
        fenced(&mut store, &reserve(0, "r", 2, 3), "reserve", 0);
        let quantity = request(0, resolution_operation(kind, QuantityResolution::All));
        let now = if kind == "expire" { 4 } else { 0 };
        let first = fenced(&mut store, &quantity, "resolve", now);
        let original = first
            .receipt()
            .body
            .quantity
            .as_ref()
            .expect("retained request");
        let before = store.read().dump().expect("before retry");
        let replay = commit(&mut store, original, "resolve", 0).expect("exact retry");
        assert!(matches!(replay, CommitOutcome::Replayed(_)));
        assert_eq!(first.receipt(), replay.receipt());
        let mut changed = original.clone();
        changed.balance_versions.clear();
        assert!(matches!(
            commit(&mut store, &changed, "resolve", now),
            Err(CellError::IdempotencyConflict(_))
        ));
        assert_eq!(before, store.read().dump().expect("unchanged retry"));
        assert_refusal(
            &mut store,
            &quantity,
            now,
            &if kind == "post" {
                QuantityError::Duplicate
            } else {
                QuantityError::Terminal
            },
        );
        if kind == "expire" {
            assert!(matches!(
                commit(&mut store, &quantity, "regressed", 3),
                Err(CellError::Quantity(QuantityError::ClockRegression))
            ));
            assert_eq!(
                before,
                store.read().dump().expect("unchanged clock refusal")
            );
        }
    }
    let mut store = initialized();
    fenced(&mut store, &reserve(0, "r", 2, 3), "reserve", 0);
    for kind in ["post", "void"] {
        let invalid = request(0, resolution_operation(kind, subset(3)));
        assert_refusal(&mut store, &invalid, 3, &QuantityError::InvalidContract);
        let valid = request(0, resolution_operation(kind, subset(1)));
        let before = store.read().dump().expect("before deadline");
        let mut valid = valid;
        valid.balance_versions = store
            .read()
            .quantity_balance_versions("main", &valid)
            .expect("live dependencies");
        assert!(matches!(
            commit(&mut store, &valid, "deadline", 3),
            Err(CellError::Quantity(QuantityError::Expired))
        ));
        assert_eq!(before, store.read().dump().expect("unchanged deadline"));
    }
    let quantity = request(0, resolution_operation("post", QuantityResolution::All));
    let mut state = pure_state(&store, &quantity);
    state.reservation.as_mut().expect("open").remaining.clear();
    assert_eq!(
        prepare_quantity(&quantity, &state, 0).expect_err("empty open remainder"),
        QuantityError::InvalidContract
    );
    state.version = u64::MAX;
    state.identity_exists = true;
    state.reservation = None;
    assert_eq!(
        quantity_balance_keys(&quantity, &state),
        Err(QuantityError::Overflow)
    );
    assert_eq!(
        prepare_quantity(&quantity, &state, 0).expect_err("revision overflow"),
        QuantityError::Overflow
    );
}

#[test]
fn forged_reservation_status_and_remainder_refuse_before_terminal_or_duplicate() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    for status in ["open", "posted", "void", "expired"] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
        store.bind("scope", "tenant/dev").expect("scope");
        fenced(&mut store, &define(), "define", 0);
        fenced(&mut store, &post(0, "fund", "fund", 10), "fund", 0);
        fenced(&mut store, &reserve(0, "r", 2, 3), "reserve", 0);
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        let mut reservation = store
            .read()
            .quantity_reservation("main", "stock", "r")
            .expect("read")
            .expect("record");
        reservation.status = status.into();
        reservation.remaining.clear();
        let record = String::from_utf8(
            norn_ir::canonical::canonical_json_of(&reservation).expect("canonical"),
        )
        .expect("UTF-8");
        engine
            .execute(
                "UPDATE _norn_quantity_reservation SET record=?1 WHERE id='r'",
                &[record.into()],
            )
            .expect("damage cache");
        let before = store.read().dump().expect("damaged dump");
        for kind in ["post", "void", "expire"] {
            let mut quantity = request(0, resolution_operation(kind, subset(1)));
            if let QuantityOperation::PostReservation { posting_id, .. } = &mut quantity.operation {
                *posting_id = "fund".into(); // Duplicate must not mask forged provenance.
            }
            assert!(matches!(
                store.read().quantity_balance_versions("main", &quantity),
                Err(CellError::Quantity(QuantityError::Corrupt))
            ));
            assert!(matches!(
                commit(&mut store, &quantity, "corrupt", 4),
                Err(CellError::Quantity(QuantityError::Corrupt))
            ));
            assert_eq!(
                before,
                store.read().dump().expect("unchanged damaged state")
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "bounded checked-in corpus vectors and known fixture pack positions"
)]
fn all_sixteen_quint_terminal_mismatches_are_retained_as_reduced_trace_regressions() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/quantity-terminal-quint.json"))
            .expect("corpus");
    let cases = corpus["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 16);
    for case in cases {
        let original = &case["reserve"];
        let target = &case["request"];
        let n = |record: &serde_json::Value, field: &str| {
            record[field].as_i64().expect("bounded trace integer")
        };
        let slot = n(original, "slot");
        let partition = n(original, "partition");
        let source = format!("lot{slot}");
        let unit = format!("u{}", partition / 2);
        let layer = format!("l{}", partition % 2);
        let reservation_id = format!("r{slot}");
        let mut definition = define();
        if let QuantityOperation::Define { pack, .. } = &mut definition.operation {
            pack.units[0].unit_id.clone_from(&unit);
            pack.layers[0].layer_id.clone_from(&layer);
            for account in &mut pack.accounts {
                account.allowed_unit_ids = vec![unit.clone()];
                if account.account_id == "lot" {
                    account.account_id.clone_from(&source);
                    account.funding = if slot == 0 {
                        LedgerFundingPolicy::Gross
                    } else {
                        LedgerFundingPolicy::Net
                    };
                } else if account.account_id == "transit" {
                    account.account_id = "dest".into();
                }
            }
            for template in &mut pack.templates {
                for leg in &mut template.legs {
                    leg.unit_id.clone_from(&unit);
                    leg.layer.clone_from(&layer);
                    if leg.account_id == "lot" {
                        leg.account_id.clone_from(&source);
                    } else if leg.account_id == "transit" {
                        leg.account_id = "dest".into();
                    }
                }
            }
            let mut move_template = pack.templates[1].clone();
            move_template.template_id = format!("move{partition}-{slot}");
            move_template.parameters.clear();
            let mut back = move_template.legs[0].clone();
            back.leg_id = "back".into();
            move_template.legs[1].leg_id = "dest".into();
            move_template.legs.insert(1, back);
            for (leg, amount) in move_template.legs.iter_mut().zip([
                -n(original, "out"),
                n(original, "back"),
                n(original, "dest"),
            ]) {
                leg.amount.terms.clear();
                leg.amount.constant_unscaled = amount.to_string();
            }
            pack.templates.push(move_template);
        }
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store.bind("scope", "tenant/dev").expect("scope");
        fenced(&mut store, &definition, "define", 0);
        fenced(&mut store, &post(0, "fund", "fund", 2), "fund", 0);
        let quantity = request(
            0,
            QuantityOperation::Reserve {
                reservation_id: reservation_id.clone(),
                template_id: format!("move{partition}-{slot}"),
                parameters: vec![],
                expires_at: 3,
            },
        );
        fenced(
            &mut store,
            &quantity,
            "reserve",
            u64::try_from(n(original, "now")).expect("time"),
        );
        let expire = request(
            0,
            QuantityOperation::Expire {
                reservation_id: reservation_id.clone(),
            },
        );
        fenced(&mut store, &expire, "expire", 4);
        let posting = target["kind"] == "post_slice";
        let resolution = QuantityResolution::Subset {
            legs: [
                ("out", -n(target, "out")),
                ("back", n(target, "back")),
                ("dest", n(target, "dest")),
            ]
            .into_iter()
            .filter(|(_, amount)| *amount != 0)
            .map(|(id, amount)| QuantitySlice {
                leg_id: id.into(),
                amount_unscaled: amount.to_string(),
            })
            .collect(),
        };
        let mut quantity = request(
            0,
            if posting {
                QuantityOperation::PostReservation {
                    reservation_id: reservation_id.clone(),
                    posting_id: "fresh-post".into(),
                    resolution,
                }
            } else {
                QuantityOperation::Void {
                    reservation_id: reservation_id.clone(),
                    resolution,
                }
            },
        );
        for (account, field, include) in [
            (
                source.as_str(),
                "sourceVersion",
                n(target, "out") - n(target, "back") > 0,
            ),
            ("dest", "destVersion", posting && n(target, "dest") != 0),
        ] {
            if include {
                quantity.balance_versions.push(QuantityBalanceVersion {
                    account: account.to_owned(),
                    unit: unit.clone(),
                    layer: layer.clone(),
                    version: u64::try_from(n(target, field)).expect("version"),
                });
            }
        }
        let closed = store
            .read()
            .quantity_reservation("main", "stock", &reservation_id)
            .expect("read")
            .expect("record");
        assert_eq!(closed.status, "expired");
        assert!(closed.remaining.is_empty());
        assert_eq!(case["outcome"], "terminal");
        let state = QuantityState {
            reservation: Some(closed),
            ..QuantityState::default()
        };
        assert_eq!(
            quantity_balance_keys(&quantity, &state),
            Err(QuantityError::Terminal)
        );
        assert_eq!(
            prepare_quantity(&quantity, &state, 4).expect_err("pure terminal"),
            QuantityError::Terminal
        );
        let before = store.read().dump().expect("before");
        assert!(matches!(
            store.read().quantity_balance_versions("main", &quantity),
            Err(CellError::Quantity(QuantityError::Terminal))
        ));
        assert!(
            matches!(
                commit(&mut store, &quantity, "fresh", 4),
                Err(CellError::Quantity(QuantityError::Terminal))
            ),
            "{}:{}",
            case["trace"],
            case["stateIndex"]
        );
        assert_eq!(
            before,
            store.read().dump().expect("unchanged corpus refusal")
        );
    }
}

#[test]
fn terminal_refusal_preserves_marked_facts_and_quantity_history_rebuild() {
    use norn_cell::FactWriteRow;
    use norn_cell::engine::NativeEngine;
    let definition = serde_json::from_value(serde_json::json!({
        "name":"fixture.Private",
        "properties":[{"id":"value","type":{"kind":"text"},"markings":["pii.contact"]}]
    }))
    .expect("marked definition");
    let model = Model::from_definitions(&[definition], &[]).expect("marked model");
    let mut store = CellStore::open_in_memory(model).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    fenced(&mut store, &define(), "define", 0);
    fenced(&mut store, &post(0, "fund", "fund", 10), "fund", 0);
    fenced(&mut store, &reserve(0, "r", 2, 3), "reserve", 0);
    let quantity = request(0, resolution_operation("void", QuantityResolution::All));
    let mut quantity = quantity;
    quantity.balance_versions = store
        .read()
        .quantity_balance_versions("main", &quantity)
        .expect("live dependencies");
    let mixed = CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        idempotency_key: Some("classified-resolution".into()),
        quantity: Some(quantity),
        writes: vec![FactWriteRow::Put {
            object_type: "fixture.Private".into(),
            key: "private".into(),
            values: std::collections::BTreeMap::from([(
                "value".into(),
                norn_ir::RuntimeValue::from("classified"),
            )]),
        }],
        ..CommitBatch::default()
    };
    let first = store.commit(&mixed).expect("atomic marked resolution");
    assert_eq!(
        store.commit(&mixed).expect("exact retry").receipt(),
        first.receipt()
    );
    let before = store.read().dump().expect("before refusal");
    let refused = CommitBatch {
        idempotency_key: Some("terminal-marked-write".into()),
        quantity: Some(request(0, resolution_operation("post", subset(3)))),
        ..mixed
    };
    assert!(matches!(
        store.commit(&refused),
        Err(CellError::Quantity(QuantityError::Terminal))
    ));
    assert_eq!(before, store.read().dump().expect("atomic refusal"));
    let records = (1..=4)
        .map(|seq| store.export_record(seq).expect("record").expect("present"))
        .collect::<Vec<_>>();
    let restored = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &records,
        &records.last().expect("head").hash().expect("hash"),
    )
    .expect("quantity inputs and marking proofs replay together");
    assert_eq!(before, restored.read().dump().expect("restored state"));
    for selected in [&store, &restored] {
        assert_eq!(
            selected
                .read()
                .effective_markings("main", "fixture.Private", "private", Some(1))
                .expect("markings")
                .expect("proof")
                .get("value"),
            Some(&vec!["pii.contact".to_owned()])
        );
        selected.read().verify_ledger().expect("ledger and proofs");
    }
}
