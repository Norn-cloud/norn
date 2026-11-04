//! ID-COLLISION / ID-UNIQUE-1: original values stay stored; normalized tuples are unique.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native identity conformance fixtures"
)]
mod common;
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, Model, SqliteCell, StoreOptions};
use norn_ir::{
    ComponentNormalization, IdentityRule, KeyName, KeySpec, Normalization, ObjectTypeDef,
    ObjectTypeName, PropertyId, PropertyType, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite, ObservationEvent,
};
use norn_testkit::proptest::prelude::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

fn definitions() -> Vec<ObjectTypeDef> {
    let mut root = common::object_types()
        .into_iter()
        .find(|d| d.name.as_str() == "projects.Project")
        .expect("typed fixture");
    root.name = ObjectTypeName("identity.Root".into());
    root.links.clear();
    root.description = None;
    root.properties.retain(|p| p.id.as_str() == "code");
    root.properties[0].property_type = PropertyType::Text { format: None };
    root.properties[0].constraints = norn_ir::PropertyConstraints::default();
    root.identity = vec![KeySpec {
        name: KeyName("primary".into()),
        properties: vec![PropertyId("code".into())],
        naming_series: None,
        rule: Some(IdentityRule {
            normalize: vec![ComponentNormalization {
                property: PropertyId("code".into()),
                steps: vec![
                    Normalization::Trim,
                    Normalization::Nfkc,
                    Normalization::Uppercase,
                ],
            }],
            match_policy: None,
        }),
    }];
    let mut child = root.clone();
    child.name = ObjectTypeName("identity.Child".into());
    child.extends = Some(root.name.clone());
    child.properties.clear();
    child.identity.clear();
    let mut sibling = child.clone();
    sibling.name = ObjectTypeName("identity.Sibling".into());
    vec![root, child, sibling]
}
fn model() -> Model {
    Model::from_definitions(&definitions(), &[]).expect("identity model")
}
fn put(ty: &str, id: &str, code: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: ty.into(),
        key: id.into(),
        values: BTreeMap::from([("code".into(), RuntimeValue::from(code))]),
    }
}
fn batch(scope: &str, world: &str, writes: Vec<FactWriteRow>) -> CommitBatch {
    CommitBatch {
        scope: scope.into(),
        world: world.into(),
        writes,
        ..CommitBatch::default()
    }
}

type IdentityCell = SqliteCell<ObservationEvent, JsonObject, JsonObject>;
fn identity_cell(path: &std::path::Path) -> IdentityCell {
    IdentityCell::open(path, model(), StoreOptions::default(), "org_a", |_| {
        JsonObject::new()
    })
    .expect("identity cell")
}
fn identity_proposal(
    cell: &IdentityCell,
    code: Option<&str>,
) -> CellCommit<ObservationEvent, JsonObject, JsonObject> {
    let key = FactKey::new("identity.Child", "stable");
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        expected: cell.load(&key).expect("current identity").version,
        key,
        mutation: code.map_or(FactMutation::Delete, |code| FactMutation::Put {
            values: BTreeMap::from([("code".into(), RuntimeValue::from(code))]),
        }),
    });
    commit
}

/// ID-AUTHORED-RECREATE / ID-TOMBSTONE-REPLAY: native and canonical Creates refuse atomically.
#[test]
fn authored_identity_reuse_refuses_before_writes_and_after_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("authored.sqlite");
    let mut cell = identity_cell(&path);
    cell.commit(identity_proposal(&cell, Some("original")))
        .expect("create");
    cell.commit(identity_proposal(&cell, None)).expect("delete");
    let proposal = identity_proposal(&cell, Some("changed business key"));
    let before = cell.store().read().dump().expect("before");
    for _ in 0..2 {
        let refusal = cell.commit(proposal.clone()).expect_err("restore required");
        assert_eq!(refusal.code(), norn_checker::IDENTITY_REUSE);
        assert_eq!(refusal.to_string(), norn_checker::IDENTITY_REUSE_MESSAGE);
        assert_eq!(cell.store().read().dump().expect("unchanged"), before);
        drop(cell);
        cell = identity_cell(&path);
    }
    drop(cell);
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("raw store");
    let mut canonical = batch("org_a", "main", Vec::new());
    canonical.facts = ["fresh", "stable"]
        .into_iter()
        .map(|key| norn_ir::FactChange::Create {
            object_type: ObjectTypeName("identity.Child".into()),
            key: key.into(),
            values: BTreeMap::from([("code".into(), RuntimeValue::from(key))]),
        })
        .collect();
    let refusal = store.commit(&canonical).expect_err("canonical recreation");
    assert!(matches!(refusal, CellError::IdentityReuse));
    assert_eq!(store.read().dump().expect("canonical refusal"), before);
    let embedding = put("identity.Child", "stable", "embedding");
    store
        .commit(&batch("org_a", "main", vec![embedding]))
        .expect("embedding");
    let committed = store.read().dump().expect("raw committed");
    let records = (1..=3)
        .map(|seq| store.export_record(seq).expect("export").expect("record"))
        .collect::<Vec<_>>();
    let engine = norn_cell::engine::NativeEngine::open_in_memory().expect("engine");
    let head = records[2].hash().expect("retained head");
    let rebuilt = CellStore::rebuild_export_history(engine, &records, &head).expect("rebuild");
    assert_eq!(rebuilt.read().dump().expect("replayed"), committed);
    let mut rebuilt = IdentityCell::new(rebuilt, "org_a", |_| JsonObject::new()).expect("host");
    rebuilt
        .commit(identity_proposal(&rebuilt, None))
        .expect("new authored delete");
    let replayed = rebuilt.store().read().dump().expect("after delete");
    let refusal = rebuilt
        .commit(identity_proposal(&rebuilt, Some("new")))
        .expect_err("reuse");
    assert_eq!(refusal.code(), norn_checker::IDENTITY_REUSE);
    assert_eq!(rebuilt.store().read().dump().expect("unchanged"), replayed);
    drop(store);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen raw");
    assert_eq!(store.read().dump().expect("raw semantics"), committed);
}

fn identity_observation(
    cell: &IdentityCell,
    delivery: &str,
) -> norn_kernel::snapshot_observe::PreparedObservation {
    use norn_kernel::snapshot_observe::{ObservedSnapshot, prepare_observed_snapshot};
    use norn_kernel::{DecisionTurn, LogicalTime, TypeCatalog, World, load_slice};
    let definitions = definitions();
    let catalog = TypeCatalog::new(&definitions);
    let key = FactKey::new("identity.Child", "stable");
    let slice = load_slice(cell, [key.clone()]).expect("observation slice");
    let values = BTreeMap::from([("code".into(), RuntimeValue::from("observed"))]);
    let provenance = serde_json::from_value(serde_json::json!({
        "source":{"system":"synthetic", "record":"identity/stable"},
        "mappingVersion":"fixture.identity@1", "recordedAt":"2026-11-01T00:00:00Z", "sourceFields":["code"]
    })).expect("source provenance");
    prepare_observed_snapshot(
        &catalog,
        &slice,
        &DecisionTurn {
            id: delivery.into(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("sealed observed write")
}

/// ID-OBSERVED-REAPPEAR: observation events alone cannot replace the sealed per-write authority.
#[test]
fn observed_reappearance_retains_identity_and_advances_its_version() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("observed.sqlite");
    let mut cell = identity_cell(&path);
    let (commit, authority) = identity_observation(&cell, "first").into_parts();
    cell.commit_observed(commit, authority)
        .expect("first observation");
    cell.commit(identity_proposal(&cell, None))
        .expect("committed deletion");
    let key = FactKey::new("identity.Child", "stable");
    let deleted = cell.load(&key).expect("tombstone").version;
    let (commit, authority) = identity_observation(&cell, "reappear").into_parts();
    let before = cell.store().read().dump().expect("before reappearance");
    let refusal = cell.commit(commit.clone()).expect_err("unsealed");
    assert_eq!(refusal.code(), norn_checker::IDENTITY_REUSE);
    assert_eq!(cell.store().read().dump().expect("unchanged"), before);
    cell.commit_observed(commit, authority).expect("reappear");
    let reappeared = cell.load(&key).expect("same identity");
    assert!(reappeared.exists());
    assert!(reappeared.version > deleted);
    let after = cell.store().read().dump().expect("observed state");
    drop(cell);
    let cell = identity_cell(&path);
    let reopened = cell.load(&key).expect("retained version").version;
    assert_eq!(reopened, reappeared.version);
    assert_eq!(cell.store().read().dump().expect("stable reopen"), after);
}

proptest! {
    #[test]
    fn observed_version_successor_lifts_retained_deletion(version in 1_u64..u64::MAX) {
        let deleted = FactVersion(version);
        prop_assert!(deleted.has_committed_deletion(false));
        let appeared = deleted.checked_next().expect("bounded successor");
        prop_assert!(appeared > deleted);
        prop_assert!(!appeared.has_committed_deletion(true));
        prop_assert!(!FactVersion::ABSENT.has_committed_deletion(false));
    }
}
#[test]
fn id_collision_between_sibling_types_rolls_back_the_complete_commit() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Child", "first", " abC ")],
        ))
        .expect("first identity");
    let before = store.read().dump().expect("before");
    let mut same_identity = batch("org_a", "main", Vec::new());
    same_identity.facts.push(norn_ir::FactChange::Create {
        object_type: ObjectTypeName("identity.Sibling".into()),
        key: "first".into(),
        values: BTreeMap::from([("code".into(), RuntimeValue::from("other"))]),
    });
    // A live sibling is not a deletion; preserve the existing duplicate-row refusal.
    let collision = store.commit(&same_identity).expect_err("live sibling");
    assert!(matches!(collision, CellError::Sqlite(_)));
    assert_eq!(store.read().dump().expect("live sibling refusal"), before);
    let error = store
        .commit(&batch(
            "org_a",
            "main",
            vec![
                put("identity.Sibling", "other", "other"),
                put("identity.Sibling", "second", "ＡＢＣ"),
            ],
        ))
        .expect_err("normalized collision");
    assert!(
        matches!(error, CellError::IdentityCollision { ref owner, ref key, .. } if owner == "identity.Root" && key == "primary")
    );
    assert_eq!(store.read().dump().expect("after"), before);
    let value = store
        .read()
        .object("main", "identity.Root", "first")
        .expect("read")
        .expect("object");
    assert_eq!(value.values.get("code"), Some(&RuntimeValue::from(" abC ")));
}
#[test]
fn identity_key_changes_keep_ids_and_remain_scoped_and_world_bound() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    for (scope, world, id) in [
        ("org_a", "main", "a"),
        ("org_b", "main", "b"),
        ("org_a", "fork", "c"),
    ] {
        store
            .commit(&batch(
                scope,
                world,
                vec![put("identity.Child", id, " abc ")],
            ))
            .expect("separate domain");
    }
    let before = store
        .read()
        .object("main", "identity.Child", "a")
        .expect("read")
        .expect("object");
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Child", "a", "renamed")],
        ))
        .expect("rename");
    let after = store
        .read()
        .object("main", "identity.Child", "a")
        .expect("read")
        .expect("object");
    assert_eq!(before.id, after.id);
    assert_eq!(
        after.values.get("code"),
        Some(&RuntimeValue::from("renamed"))
    );
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Sibling", "d", "ABC")],
        ))
        .expect("old business key released");
}
#[test]
fn concurrent_normalized_creates_have_one_winner_and_reopen_keeps_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("identity.sqlite");
    drop(CellStore::open(&path, model(), StoreOptions::default()).expect("initialize"));
    let barrier = Arc::new(Barrier::new(2));
    let workers = [("a", " abc "), ("b", "ABC")].map(|(id, code)| {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let mut store =
                CellStore::open(&path, model(), StoreOptions::default()).expect("open writer");
            barrier.wait();
            store.commit(&batch(
                "org_a",
                "main",
                vec![put("identity.Child", id, code)],
            ))
        })
    });
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("join"))
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .all(|error| matches!(error, CellError::IdentityCollision { .. }))
    );
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen");
    let (rows, complete) = store
        .read()
        .root_rows("main", "identity.Root", 10)
        .expect("rows");
    assert!(complete);
    assert_eq!(rows.len(), 1);
}
#[test]
fn normalized_scan_row_limit_refuses_atomically() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    let before = store.read().dump().expect("before");
    let writes = (0..4097)
        .map(|n| put("identity.Child", &format!("id{n}"), &format!("code{n}")))
        .collect();
    let error = store
        .commit(&batch("org_a", "main", writes))
        .expect_err("row limit");
    assert!(matches!(
        error,
        CellError::OutOfRange("normalized identity scan rows")
    ));
    assert_eq!(store.read().dump().expect("after"), before);
}

#[test]
fn unrelated_key_domains_cannot_commit_beyond_the_reopen_budget() {
    let mut defs = definitions();
    let key = defs[0].identity[0].clone();
    defs[0].identity = (0..64)
        .map(|n| norn_ir::KeySpec {
            name: KeyName(format!("key{n}")),
            ..key.clone()
        })
        .collect();
    let mut other = defs[0].clone();
    other.name = ObjectTypeName("identity.Other".into());
    defs.push(other);
    let model = Model::from_definitions(&defs, &[]).expect("two identity domains");
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("budget.sqlite");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    let writes = (0..33)
        .map(|n| put("identity.Root", &format!("a{n}"), &format!("A{n}")))
        .collect();
    store
        .commit(&batch("org_a", "main", writes))
        .expect("first domain");
    let before = store.read().dump().expect("before");
    let writes = (0..33)
        .map(|n| put("identity.Other", &format!("b{n}"), &format!("B{n}")))
        .collect();
    assert!(matches!(
        store.commit(&batch("org_a", "main", writes)),
        Err(CellError::OutOfRange("normalized identity scan rows"))
    ));
    assert_eq!(store.read().dump().expect("after"), before);
    drop(store);
    let store = CellStore::open(&path, model, StoreOptions::default())
        .expect("accepted commits always reopen");
    assert_eq!(store.read().dump().expect("reopen"), before);
}
#[test]
fn normalization_profile_refuses_unsupported_components_and_step_exhaustion() {
    let mut defs = definitions();
    let mut amount = defs[0].properties[0].clone();
    amount.id = PropertyId("amount".into());
    amount.property_type = PropertyType::Decimal {
        additivity: norn_ir::PropertyAdditivity::Undeclared {},
        scale: norn_ir::WholeNumber(2),
        rounding: None,
        unit: None,
        currency_source: None,
        per_unit: None,
    };
    defs[0].properties.push(amount);
    defs[0].identity[0]
        .properties
        .push(PropertyId("amount".into()));
    assert!(matches!(
        Model::from_definitions(&defs, &[]),
        Err(CellError::Encoding(_))
    ));
    let mut defs = definitions();
    defs[0].identity[0]
        .rule
        .as_mut()
        .expect("normalizer")
        .normalize[0]
        .steps = vec![Normalization::Trim; 17];
    assert!(matches!(
        Model::from_definitions(&defs, &[]),
        Err(CellError::Encoding(_))
    ));
}

#[test]
fn normalized_uniqueness_retains_resolution_metadata_without_invoking_it() {
    let mut defs = definitions();
    let policy = Some(norn_ir::ResolutionPolicyName("unavailable.policy".into()));
    defs[0].identity[0]
        .rule
        .as_mut()
        .expect("normalizer")
        .match_policy = policy.clone();
    let model = Model::from_definitions(&defs, &[]).expect("existing metadata remains valid");
    assert_eq!(
        model
            .object_types()
            .find(|definition| definition.name.as_str() == "identity.Root")
            .expect("definition")
            .identity[0]
            .rule
            .as_ref()
            .expect("normalizer")
            .match_policy,
        policy
    );
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Child", "a", " abc ")],
        ))
        .expect("canonical key");
    assert!(matches!(
        store.commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Sibling", "b", "ABC")]
        )),
        Err(CellError::IdentityCollision { .. })
    ));
}
#[test]
fn normalized_composite_keys_keep_typed_tuple_boundaries() {
    let mut defs = definitions();
    let mut suffix = defs[0].properties[0].clone();
    suffix.id = PropertyId("suffix".into());
    defs[0].properties.push(suffix);
    defs[0].identity[0]
        .properties
        .push(PropertyId("suffix".into()));
    let mut store =
        CellStore::open_in_memory(Model::from_definitions(&defs, &[]).expect("composite model"))
            .expect("store");
    let composite = |id: &str, code: &str, suffix: &str| {
        let mut row = put("identity.Child", id, code);
        if let FactWriteRow::Put { values, .. } = &mut row {
            values.insert("suffix".into(), RuntimeValue::from(suffix));
        }
        row
    };
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![composite("a", " ab ", "c"), composite("b", " a ", "bc")],
        ))
        .expect("different tuples");
    assert!(matches!(
        store.commit(&batch("org_a", "main", vec![composite("c", "ＡＢ", "c")])),
        Err(CellError::IdentityCollision { .. })
    ));
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn generated_normalized_variants_never_commit_a_duplicate(word in "[a-z]{1,24}", padding in 0usize..8) {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        let padded = format!("{}{}{}", " ".repeat(padding), word, " ".repeat(padding));
        prop_assert_eq!(norn_checker::normalize_text(&padded, &[Normalization::Trim, Normalization::Uppercase]), word.to_ascii_uppercase());
        store.commit(&batch("org_a", "main", vec![put("identity.Child", "a", &padded)])).expect("first");
        let before = store.read().dump().expect("before");
        let refusal = store.commit(&batch("org_a", "main", vec![put("identity.Sibling", "b", &word.to_uppercase())]));
        prop_assert!(matches!(refusal, Err(CellError::IdentityCollision { .. })), "normalized duplicate must refuse");
        prop_assert_eq!(store.read().dump().expect("after"), before);
    }
}

#[test]
fn normalized_key_refusal_keeps_its_machine_code_through_the_cell_host() {
    use norn_kernel::{
        CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite, Refusable,
    };
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Child", "first", " abc ")],
        ))
        .expect("first");
    let mut cell = norn_cell::SqliteCell::<RuntimeValue, RuntimeValue, RuntimeValue>::new(
        store,
        "org_a",
        |command| command.payload.clone(),
    )
    .expect("host");
    let before = cell.store().read().dump().expect("before");
    let error = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("identity.Sibling", "second"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: BTreeMap::from([("code".into(), RuntimeValue::from("ABC"))]),
                },
            }],
            ..CellCommit::default()
        })
        .expect_err("collision");
    assert_eq!(error.code(), "core.identity.collision");
    assert!(!error.refusal().retryable);
    assert_eq!(cell.store().read().dump().expect("after"), before);
}

#[test]
fn raw_duplicate_keeps_its_existing_refusal_and_is_atomic() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store
        .commit(&batch(
            "org_a",
            "main",
            vec![put("identity.Child", "a", "ABC")],
        ))
        .expect("first");
    let before = store.read().dump().expect("before");
    assert!(
        store
            .commit(&batch(
                "org_a",
                "main",
                vec![put("identity.Sibling", "b", "ABC")]
            ))
            .is_err()
    );
    assert_eq!(store.read().dump().expect("after"), before);
}
#[test]
fn normalized_composite_integer_key_keeps_values_beyond_binary64_precision() {
    let mut defs = definitions();
    let mut ordinal = defs[0].properties[0].clone();
    ordinal.id = PropertyId("ordinal".into());
    ordinal.property_type = PropertyType::Integer {
        additivity: norn_ir::PropertyAdditivity::Undeclared {},
    };
    defs[0].properties.push(ordinal);
    defs[0].identity[0]
        .properties
        .push(PropertyId("ordinal".into()));
    let mut store = CellStore::open_in_memory(Model::from_definitions(&defs, &[]).expect("model"))
        .expect("store");
    let row = |id: &str, code: &str, ordinal: i64| {
        let mut row = put("identity.Child", id, code);
        if let FactWriteRow::Put { values, .. } = &mut row {
            values.insert(
                "ordinal".into(),
                RuntimeValue::from_json_text(ordinal.to_string()).expect("exact integer"),
            );
        }
        row
    };
    store
        .commit(&batch("org_a", "main", vec![row("a", " abc ", i64::MAX)]))
        .expect("first");
    store
        .commit(&batch("org_a", "main", vec![row("b", "ABC", i64::MAX - 1)]))
        .expect("adjacent full-range integers stay distinct");
    assert!(matches!(
        store.commit(&batch("org_a", "main", vec![row("c", "ABC", i64::MAX)])),
        Err(CellError::IdentityCollision { .. })
    ));
}
