//! Synthetic source updates exercise atomicity, hidden-state retention and edge cascades.
#![allow(clippy::expect_used, reason = "bounded regression fixtures")]
use std::collections::BTreeSet;
use welldb_ivm::{
    Access, Batch, ClassId, MetricError, ObjectKey, Op, Record, Snapshot, SourceState, Value,
};
fn key(id: &str) -> ObjectKey {
    ObjectKey {
        object_type: "synthetic.Invoice".into(),
        id: id.into(),
    }
}
fn batch(ops: Vec<Op>) -> Batch {
    Batch {
        seq: 2,
        previous: "synthetic-1".into(),
        hash: "synthetic-2".into(),
        definition_version: 1,
        main_world: true,
        ops,
    }
}
#[test]
fn failed_atomic_batch_preserves_objects_and_head_while_non_main_only_advances_head() {
    let mut source = SourceState::from_snapshot(Snapshot {
        seq: 1,
        hash: "synthetic-1".into(),
        ..Snapshot::default()
    });
    let before = source.clone();
    let invalid = batch(vec![
        Op::MeasurePut(key("a"), Record::default()),
        Op::Set {
            key: key("missing"),
            property: "amount".into(),
            value: Value::Bool(false),
        },
    ]);
    assert_eq!(source.apply_batch(&invalid), Err(MetricError::InvalidField));
    assert_eq!(source, before);
    let mut branch = invalid;
    branch.main_world = false;
    source
        .apply_batch(&branch)
        .expect("non-main operations ignored");
    assert_eq!(source.snapshot().seq, 2);
    assert_eq!(source.snapshot().hash, "synthetic-2");
    assert!(source.snapshot().objects.is_empty());
}
#[test]
fn deleting_a_dependency_cascades_only_its_edges_and_preserves_tombstones() {
    let mut source = SourceState::default();
    let edge = |s: &str, t: &str| Op::LinkAdd {
        source: key(s),
        role: "customer".into(),
        target: key(t),
    };
    for op in [
        edge("a", "b"),
        edge("b", "a"),
        edge("b", "b"),
        edge("c", "d"),
        Op::Tombstone {
            key: key("b"),
            set: true,
        },
    ] {
        source.apply(&op).expect("setup");
    }
    // Rebuild derives the incoming index rather than retaining stale process state.
    let mut source = SourceState::from_snapshot(source.into_snapshot());
    source.apply(&Op::TargetDelete(key("b"))).expect("cascade");
    assert_eq!(source.snapshot().links.len(), 1);
    assert_eq!(
        source.snapshot().links.get(&(key("c"), "customer".into())),
        Some(&BTreeSet::from([key("d")]))
    );
    assert!(source.snapshot().tombstones.contains(&key("b")));
    source
        .apply(&Op::MeasurePut(key("b"), Record::default()))
        .expect("put");
    assert!(source.snapshot().tombstones.contains(&key("b")));
}
#[test]
fn hidden_records_remain_available_for_later_access_changes_and_writes_get_current_provenance() {
    let mut source = SourceState::default();
    source.set_head(4, "synthetic-4");
    source
        .apply(&Op::MeasurePut(key("a"), Record::default()))
        .expect("put");
    let class = ClassId {
        actor: "synthetic-analyst".into(),
        epoch: 1,
    };
    source
        .apply(&Op::Access {
            key: key("a"),
            class: class.clone(),
            access: Access {
                visible: true,
                masked: BTreeSet::new(),
            },
        })
        .expect("access");
    let record = source.snapshot().objects.get(&key("a")).expect("retained");
    assert!(record.access.get(&class).expect("class").visible);
    assert_eq!(record.seq, 4);
    assert_eq!(record.hash, "synthetic-4");
    assert_eq!(
        source.apply(&Op::EpochChange {
            actor: class.actor,
            epoch: 2
        }),
        Err(MetricError::Rebuilding)
    );
}
