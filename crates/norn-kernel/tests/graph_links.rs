#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded integration-test fixtures"
)]
//! GRAPH-LINK-1 and GRAPH-DRAFT: one transaction validates the final graph.

use norn_ir::ObjectTypeDef;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, CommitError, Effect, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LinkMutation, LinkWrite,
};
use norn_testkit::cell_host::{MemoryCell, law_command};
use norn_testkit::proptest::prelude::*;
use serde_json::{Value, json};

type Cell = MemoryCell<u8, u8, u8>;

fn definition(value: Value) -> ObjectTypeDef {
    serde_json::from_value(value).expect("valid object definition")
}

fn types() -> Vec<ObjectTypeDef> {
    let participant = |id, required, from| {
        json!({
            "id": id, "target": "graph.Resource",
            "cardinality": {"to": "one", "from": from, "required": required}
        })
    };
    vec![
        definition(json!({"name": "graph.Resource"})),
        definition(
            json!({"name": "graph.Happening", "role": "happening", "links": [
                participant("source", true, "many"), participant("destination", true, "many")
            ]}),
        ),
        definition(
            json!({"name": "graph.Owner", "links": [participant("resource", false, "one")]}),
        ),
        definition(json!({"name": "graph.Child", "extends": "graph.Owner"})),
        definition(
            json!({"name": "graph.Other", "links": [participant("resource", false, "one")]}),
        ),
    ]
}

fn put(ty: &str, id: &str) -> FactWrite {
    FactWrite {
        key: FactKey::new(ty, id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::default(),
        },
    }
}

fn edge(ty: &str, id: &str, role: &str, target: &str, add: bool) -> LinkWrite {
    LinkWrite {
        key: LinkKey::new(
            FactKey::new(ty, id),
            role,
            FactKey::new("graph.Resource", target),
        ),
        expected: FactVersion::ABSENT,
        mutation: if add {
            LinkMutation::Add {
                values: JsonObject::default(),
            }
        } else {
            LinkMutation::Remove
        },
    }
}

fn cell() -> Cell {
    Cell::new(|_| 0).with_types(types())
}

fn draft() -> CellCommit<u8, u8, u8> {
    CellCommit {
        writes: vec![
            put("graph.Happening", "h"),
            put("graph.Resource", "a"),
            put("graph.Resource", "b"),
        ],
        links: vec![
            edge("graph.Happening", "h", "source", "a", true),
            edge("graph.Happening", "h", "destination", "b", true),
        ],
        events: vec![7],
        effects: vec![Effect::Perform(law_command(0, 0, 7))],
        ..CellCommit::empty()
    }
}

#[test]
fn graph_draft_accepts_two_resources_created_in_the_same_transaction() {
    for reverse in [false, true] {
        let mut cell = cell();
        let mut commit = draft();
        if reverse {
            commit.writes.reverse();
            commit.links.reverse();
        }
        cell.commit(commit).expect("whole post-state is valid");
        assert!(
            cell.load(&FactKey::new("graph.Happening", "h"))
                .expect("load")
                .exists()
        );
        assert_eq!(cell.ledger().len(), 1);
        assert_eq!(cell.outbox().len(), 1);
    }
}

#[test]
fn graph_draft_missing_or_invalid_links_refuse_every_fact_event_and_enqueue() {
    for failure in 0..4 {
        let mut cell = cell();
        let mut commit = draft();
        match failure {
            0 => {
                commit.links.remove(0);
            }
            1 => {
                commit.links.remove(1);
            }
            2 => {
                commit.links[0].key.target = FactKey::new("graph.Happening", "h");
            }
            _ => {
                commit
                    .links
                    .push(edge("graph.Happening", "h", "destination", "a", true));
            }
        }
        assert!(cell.commit(commit).is_err());
        for key in [
            FactKey::new("graph.Happening", "h"),
            FactKey::new("graph.Resource", "a"),
            FactKey::new("graph.Resource", "b"),
        ] {
            assert!(!cell.load(&key).expect("load").exists());
        }
        for write in draft().links {
            assert!(!cell.load_link(&write.key).expect("load link").exists());
        }
        assert!(cell.ledger().is_empty());
        assert!(cell.outbox().is_empty());
    }
}

#[test]
fn graph_link_required_replacement_uses_the_final_post_state() {
    let mut cell = cell();
    cell.commit(draft()).expect("seed graph");
    let mut removal = edge("graph.Happening", "h", "destination", "b", false);
    removal.expected = FactVersion(1);
    let replace = edge("graph.Happening", "h", "destination", "a", true);
    assert!(
        cell.commit(CellCommit {
            links: vec![removal.clone()],
            ..CellCommit::empty()
        })
        .is_err()
    );
    cell.commit(CellCommit {
        links: vec![replace, removal],
        ..CellCommit::empty()
    })
    .expect("atomic replacement");
}

#[test]
fn graph_link_incoming_cardinality_distinguishes_unrelated_declarations() {
    let mut cell = cell();
    cell.commit(CellCommit {
        writes: vec![
            put("graph.Owner", "o"),
            put("graph.Other", "x"),
            put("graph.Child", "c"),
            put("graph.Resource", "t"),
        ],
        links: vec![
            edge("graph.Owner", "o", "resource", "t", true),
            edge("graph.Other", "x", "resource", "t", true),
        ],
        ..CellCommit::empty()
    })
    .expect("same name, distinct declaring relations");
    let result = cell.commit(CellCommit {
        links: vec![edge("graph.Child", "c", "resource", "t", true)],
        ..CellCommit::empty()
    });
    assert!(
        matches!(result, Err(CommitError::LinkCardinality { .. })),
        "inherited relation still has one incoming owner"
    );
}

proptest! {
    #[test]
    fn graph_link_generated_final_edges_match_declaring_relation_cardinality(
        owners in prop::collection::btree_set(0u8..6, 0..7),
        unrelated in prop::collection::btree_set(0u8..6, 0..7),
        reverse in any::<bool>(),
    ) {
        let mut cell = cell();
        let mut commit = CellCommit { writes: vec![put("graph.Resource", "t")], ..CellCommit::empty() };
        for (ty, ids) in [("graph.Owner", &owners), ("graph.Other", &unrelated)] {
            for id in ids {
                let id = id.to_string();
                commit.writes.push(put(ty, &id));
                commit.links.push(edge(ty, &id, "resource", "t", true));
            }
        }
        if reverse { commit.writes.reverse(); commit.links.reverse(); }
        let expected = owners.len() <= 1 && unrelated.len() <= 1;
        prop_assert_eq!(cell.commit(commit).is_ok(), expected);
        prop_assert_eq!(cell.load(&FactKey::new("graph.Resource", "t")).expect("load").exists(), expected);
    }
}
