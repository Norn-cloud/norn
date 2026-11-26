#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "focused MemoryCell regression"
)]

//! Bounded link snapshots retain both concrete endpoint types.

use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite,
};
use norn_testkit::cell_host::{LAW_OWNER, MemoryCell, OVERRIDE_CHILD, override_object_types};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

#[test]
fn bounded_link_snapshot_restores_concrete_target_subtype() {
    let mut cell: Cell = MemoryCell::new(|_| JsonObject::new()).with_types(override_object_types());
    let source = FactKey::new(OVERRIDE_CHILD, "source");
    let target = FactKey::new(OVERRIDE_CHILD, "target");

    cell.commit(CellCommit {
        writes: vec![
            FactWrite {
                key: source.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            },
            FactWrite {
                key: target.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            },
        ],
        ..CellCommit::empty()
    })
    .unwrap();
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(source.clone(), LAW_OWNER, target.clone()),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();

    let snapshot = cell
        .read_link_targets(&source, LAW_OWNER, 10, 1024)
        .unwrap()
        .expect("bounded snapshot is supported");
    assert_eq!(snapshot.len(), 1);
    let target_snapshot = snapshot.first().expect("one target snapshot");
    assert_eq!(
        target_snapshot.key,
        LinkKey::new(source.clone(), LAW_OWNER, target)
    );

    assert!(
        cell.read_link_targets(&source, LAW_OWNER, 0, 1024)
            .unwrap()
            .is_none(),
        "read_link_targets enforces max_targets"
    );
    assert!(
        cell.read_link_targets(&source, LAW_OWNER, 10, 2)
            .unwrap()
            .is_none(),
        "read_link_targets enforces max_key_bytes"
    );

    let predicate = norn_kernel::PredicateQuery {
        object_type: norn_ir::ObjectTypeName(OVERRIDE_CHILD.to_owned()),
        filter: std::collections::BTreeMap::new(),
        window: None,
    };
    let all = cell
        .read_predicate(&predicate, 16, u64::MAX)
        .unwrap()
        .expect("unbounded read_predicate succeeds");
    assert_eq!(all.len(), 2);
    assert!(
        cell.read_predicate(&predicate, 16, 1).unwrap().is_none(),
        "read_predicate enforces max_bytes"
    );
}
