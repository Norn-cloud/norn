//! GRAPH-DAG-1: actual production ordering against independent closure, two vertices.
#[path = "../../crates/norn-checker/src/graph.rs"]
mod graph;
use std::collections::{BTreeMap, BTreeSet};

#[kani::proof]
#[kani::unwind(12)]
fn dependency_order_refuses_exactly_cycles() {
    let bits: [bool; 4] = kani::any();
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
