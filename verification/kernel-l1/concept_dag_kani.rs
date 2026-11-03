//! GRAPH-DAG-1: production ordering over all sixteen two-vertex graphs.
#[path = "../../crates/norn-checker/src/graph.rs"]
mod graph;
use std::collections::{BTreeMap, BTreeSet};

fn check<const MASK: u8>() {
    let bits = [MASK & 1 != 0, MASK & 2 != 0, MASK & 4 != 0, MASK & 8 != 0];
    let edges: BTreeMap<_, BTreeSet<_>> = (0..2)
        .map(|i| (i, (0..2).filter(|j| bits[i * 2 + j]).collect()))
        .collect();
    let mut reach = [[false; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            reach[i][j] = bits[i * 2 + j];
        }
    }
    for k in 0..2 {
        for i in 0..2 {
            for j in 0..2 {
                reach[i][j] |= reach[i][k] && reach[k][j];
            }
        }
    }
    let (order, blocked) = graph::topological_order(&edges, |i| i);
    assert_eq!(blocked.is_empty(), !(0..2).any(|i| reach[i][i]));
    if blocked.is_empty() {
        assert_eq!(order.len(), 2);
        for (index, node) in order.iter().enumerate() {
            for dependency in &edges[node] {
                assert!(order[..index].contains(dependency));
            }
        }
    }
}

macro_rules! proofs {
    ($($name:ident = $mask:expr),* $(,)?) => {$(
        #[kani::proof]
        #[kani::unwind(12)]
        fn $name() { check::<$mask>(); }
    )*};
}
proofs!(
    edges_0 = 0,
    edges_1 = 1,
    edges_2 = 2,
    edges_3 = 3,
    edges_4 = 4,
    edges_5 = 5,
    edges_6 = 6,
    edges_7 = 7,
    edges_8 = 8,
    edges_9 = 9,
    edges_10 = 10,
    edges_11 = 11,
    edges_12 = 12,
    edges_13 = 13,
    edges_14 = 14,
    edges_15 = 15,
);
