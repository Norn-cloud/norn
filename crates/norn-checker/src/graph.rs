//! Small directed-graph helpers shared by the checks that order or bound a
//! graph: the artifact-set resolver (dependency order and cycles) and the
//! `RuleIR` v2 profile (JDM node order, cycles, and reachability).
//!
//! Vertices are `usize` indices; `edges` maps a vertex to the vertices it
//! points at. Hand-rolled on purpose: ring-0 `norn-checker` may not take a
//! graph crate (SKELETON.md), and these are the three textbook routines the
//! checks need, over graphs of at most a few thousand vertices.

use std::collections::{BTreeMap, BTreeSet};

/// Kahn's algorithm. `requires` maps every vertex (each vertex must be a key)
/// to the vertices that must come before it. Returns the vertices in order —
/// among the ready vertices, the smallest `key` first — and the vertices left
/// over, which are on or behind a cycle.
pub(crate) fn topological_order<K: Ord>(
    requires: &BTreeMap<usize, BTreeSet<usize>>,
    key: impl Fn(usize) -> K,
) -> (Vec<usize>, BTreeSet<usize>) {
    let mut pending: BTreeMap<usize, BTreeSet<usize>> = requires.clone();
    let mut unlocks: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (&vertex, before) in requires {
        for &first in before {
            unlocks.entry(first).or_default().insert(vertex);
        }
    }
    let mut ready: BTreeSet<(K, usize)> = pending
        .iter()
        .filter(|(_, before)| before.is_empty())
        .map(|(&vertex, _)| (key(vertex), vertex))
        .collect();
    let mut order = Vec::new();
    while let Some((_, vertex)) = ready.pop_first() {
        pending.remove(&vertex);
        order.push(vertex);
        for &next in unlocks.get(&vertex).into_iter().flatten() {
            if let Some(before) = pending.get_mut(&next) {
                before.remove(&vertex);
                if before.is_empty() {
                    ready.insert((key(next), next));
                }
            }
        }
    }
    (order, pending.into_keys().collect())
}

/// The cycles among `blocked` (the leftovers of [`topological_order`]): each
/// strongly connected group of two or more vertices, or one vertex with an
/// edge to itself, found by mutual reachability within `blocked`. Groups are
/// returned in the order of their smallest vertex; vertices that are only
/// behind a cycle belong to no group.
pub(crate) fn cycle_groups(
    edges: &BTreeMap<usize, BTreeSet<usize>>,
    blocked: &BTreeSet<usize>,
) -> Vec<BTreeSet<usize>> {
    let reach: BTreeMap<usize, BTreeSet<usize>> = blocked
        .iter()
        .map(|&start| (start, reachable(start, edges, blocked)))
        .collect();
    let mut grouped: BTreeSet<usize> = BTreeSet::new();
    let mut groups = Vec::new();
    for &start in blocked {
        if grouped.contains(&start) {
            continue;
        }
        let Some(from_start) = reach.get(&start) else {
            continue;
        };
        let group: BTreeSet<usize> = from_start
            .iter()
            .copied()
            .filter(|&other| {
                reach
                    .get(&other)
                    .is_some_and(|from_other| from_other.contains(&start))
            })
            .collect();
        if group.is_empty() {
            // On no cycle itself, only behind one.
            continue;
        }
        grouped.extend(group.iter().copied());
        groups.push(group);
    }
    groups
}

/// The vertices reachable from `start` in one or more steps, within `within`.
pub(crate) fn reachable(
    start: usize,
    edges: &BTreeMap<usize, BTreeSet<usize>>,
    within: &BTreeSet<usize>,
) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<usize> = edges.get(&start).into_iter().flatten().copied().collect();
    while let Some(next) = stack.pop() {
        if within.contains(&next) && seen.insert(next) {
            stack.extend(edges.get(&next).into_iter().flatten().copied());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(pairs: &[(usize, usize)], vertices: usize) -> BTreeMap<usize, BTreeSet<usize>> {
        let mut edges: BTreeMap<usize, BTreeSet<usize>> = (0..vertices)
            .map(|vertex| (vertex, BTreeSet::new()))
            .collect();
        for &(from, to) in pairs {
            edges.entry(from).or_default().insert(to);
        }
        edges
    }

    #[test]
    fn orders_acyclic_graphs_and_groups_cycles() {
        // 0 needs 1, 1 needs 2: order 2, 1, 0.
        let requires = graph(&[(0, 1), (1, 2)], 3);
        assert_eq!(
            topological_order(&requires, |v| v),
            (vec![2, 1, 0], BTreeSet::new())
        );

        // 0 <-> 1 is a cycle, 2 is behind it, 3 has a self-loop.
        let edges = graph(&[(0, 1), (1, 0), (2, 0), (3, 3)], 4);
        let (order, blocked) = topological_order(&edges, |v| v);
        assert!(order.is_empty());
        assert_eq!(
            cycle_groups(&edges, &blocked),
            vec![BTreeSet::from([0, 1]), BTreeSet::from([3])]
        );
    }
}
