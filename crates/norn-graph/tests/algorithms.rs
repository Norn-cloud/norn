//! Behavioral and independent-oracle invariants for the bounded graph library.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "small bounded test fixtures and independent brute-force oracles fail fast"
)]

use std::collections::{BTreeMap, BTreeSet};

use norn_graph::{
    Aggregation, ArithmeticError, Bom, Budget, BudgetExceeded, Composition, Decimal, Direction,
    Error, Graph, Limit, Part, Quantity, critical_path, explode, hierarchical_rollup, reachability,
    topological_order,
};
use proptest::prelude::*;

const BUDGET: Budget = Budget {
    max_nodes: 20_000,
    max_edges: 20_000,
    max_steps: 500_000,
};

fn d(value: i64) -> Decimal {
    Decimal::from(value)
}

fn graph() -> Graph<&'static str> {
    Graph {
        nodes: vec!["a", "b", "c", "d", "isolated"],
        edges: vec![("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")],
    }
}

#[test]
fn stable_order_includes_isolated_nodes_and_parallel_edges() {
    let mut graph = graph();
    graph.edges.push(("a", "b"));
    assert_eq!(
        topological_order(&graph, BUDGET).unwrap(),
        vec!["a", "b", "c", "d", "isolated"]
    );
    graph.nodes.reverse();
    graph.edges.reverse();
    assert_eq!(
        topological_order(&graph, BUDGET).unwrap(),
        vec!["a", "b", "c", "d", "isolated"]
    );
}

#[test]
fn cycles_are_concrete_closed_paths() {
    let graph = Graph {
        nodes: vec![0, 1, 2, 3],
        edges: vec![(0, 1), (1, 2), (2, 1), (2, 3)],
    };
    let Err(Error::Cycle(cycle)) = topological_order(&graph, BUDGET) else {
        unreachable!()
    };
    assert_eq!(cycle.first(), cycle.last());
    assert!(cycle.len() >= 2);
    for edge in cycle.windows(2) {
        assert!(graph.edges.contains(&(edge[0], edge[1])));
    }
    let graph = Graph {
        nodes: vec![0],
        edges: vec![(0, 0)],
    };
    assert_eq!(
        topological_order(&graph, BUDGET),
        Err(Error::Cycle(vec![0, 0]))
    );
}

#[test]
fn invalid_declarations_and_seeds_fail_closed() {
    let duplicate_graph = Graph {
        nodes: vec![1, 1],
        edges: vec![],
    };
    assert_eq!(
        topological_order(&duplicate_graph, BUDGET),
        Err(Error::DuplicateNode(1))
    );
    let invalid_graph = Graph {
        nodes: vec![1],
        edges: vec![(1, 2)],
    };
    assert_eq!(
        topological_order(&invalid_graph, BUDGET),
        Err(Error::UnknownNode(2))
    );
    assert_eq!(
        reachability(&graph(), &["unknown"], Direction::Upstream, BUDGET),
        Err(Error::UnknownNode("unknown"))
    );
}

#[test]
fn impact_unions_both_directions_and_handles_cycles() {
    let mut graph = graph();
    assert_eq!(
        reachability(&graph, &["b", "c", "b"], Direction::Downstream, BUDGET).unwrap(),
        BTreeSet::from(["b", "c", "d"])
    );
    assert_eq!(
        reachability(&graph, &["d"], Direction::Upstream, BUDGET).unwrap(),
        BTreeSet::from(["a", "b", "c", "d"])
    );
    assert!(
        reachability(&graph, &[], Direction::Downstream, BUDGET)
            .unwrap()
            .is_empty()
    );
    graph.edges.push(("d", "a"));
    assert_eq!(
        reachability(&graph, &["a"], Direction::Downstream, BUDGET).unwrap(),
        BTreeSet::from(["a", "b", "c", "d"])
    );
}

#[test]
fn critical_path_diamond_has_expected_float() {
    let graph = graph();
    let durations = BTreeMap::from([
        ("a", d(2)),
        ("b", d(5)),
        ("c", d(3)),
        ("d", d(4)),
        ("isolated", d(1)),
    ]);
    let result = critical_path(&graph, &durations, BUDGET).unwrap();
    assert_eq!(result.length, d(11));
    assert_eq!(result.timings["c"].earliest_start, d(2));
    assert_eq!(result.timings["c"].latest_start, d(4));
    assert_eq!(result.timings["c"].slack, d(2));
    assert_eq!(result.timings["isolated"].slack, d(10));
    for node in ["a", "b", "d"] {
        assert_eq!(result.timings[node].slack, d(0));
    }
}

fn bom() -> Bom<&'static str, Decimal> {
    Bom {
        parts: BTreeMap::from([
            (
                "assembly",
                Part {
                    cost: d(1),
                    lead_time: d(2),
                },
            ),
            (
                "sub",
                Part {
                    cost: d(2),
                    lead_time: d(3),
                },
            ),
            (
                "base",
                Part {
                    cost: d(5),
                    lead_time: d(4),
                },
            ),
        ]),
        lines: vec![
            Composition {
                parent: "assembly",
                child: "sub",
                quantity: d(2),
            },
            Composition {
                parent: "assembly",
                child: "base",
                quantity: d(3),
            },
            Composition {
                parent: "sub",
                child: "base",
                quantity: d(4),
            },
        ],
    }
}

#[test]
fn shared_bom_sums_diamond_demands_and_intrinsic_cost() {
    let result = explode(
        &bom(),
        &BTreeMap::from([("assembly", d(2)), ("sub", d(1))]),
        BUDGET,
    )
    .unwrap();
    assert_eq!(result.requirements, BTreeMap::from([("base", d(26))]));
    assert_eq!(result.unit_rollups["assembly"].cost, d(60));
    assert_eq!(result.total_cost, d(142));
    assert_eq!(result.lead_time, d(9));
}

#[test]
fn fractional_and_parallel_bom_lines_are_exact() {
    let mut bom = bom();
    bom.lines = vec![
        Composition {
            parent: "assembly",
            child: "base",
            quantity: Decimal::new(25, 2),
        },
        Composition {
            parent: "assembly",
            child: "base",
            quantity: Decimal::new(75, 2),
        },
    ];
    let result = explode(
        &bom,
        &BTreeMap::from([("assembly", Decimal::new(15, 1))]),
        BUDGET,
    )
    .unwrap();
    assert_eq!(result.requirements["base"], Decimal::new(15, 1));
    assert_eq!(result.total_cost, d(9));
    assert_eq!(result.lead_time, d(6));
}

#[test]
fn bom_rejects_cycles_even_outside_demand_and_zero_lines() {
    let mut bom = bom();
    bom.lines.push(Composition {
        parent: "base",
        child: "sub",
        quantity: d(0),
    });
    assert!(matches!(
        explode(&bom, &BTreeMap::new(), BUDGET),
        Err(Error::Cycle(_))
    ));
}

#[test]
fn zero_requirements_do_not_contribute_cost_or_lead_time() {
    let mut bom = bom();
    for line in &mut bom.lines {
        line.quantity = d(0);
    }
    let result = explode(&bom, &BTreeMap::from([("assembly", d(1))]), BUDGET).unwrap();
    assert!(result.requirements.is_empty());
    assert_eq!(result.total_cost, d(1));
    assert_eq!(result.lead_time, d(2));
    let result = explode(&bom, &BTreeMap::from([("assembly", d(0))]), BUDGET).unwrap();
    assert!(result.requirements.is_empty());
    assert_eq!(result.total_cost, d(0));
    assert_eq!(result.lead_time, d(0));
}

#[test]
fn tree_rollups_include_intrinsic_values_and_signed_leaves() {
    let tree = Graph {
        nodes: vec![0, 1, 2, 3],
        edges: vec![(0, 1), (0, 2), (1, 3)],
    };
    let values = BTreeMap::from([(0, d(2)), (1, d(3)), (2, d(-4)), (3, d(5))]);
    let result = hierarchical_rollup(&tree, &0, &values, Aggregation::Sum, BUDGET).unwrap();
    assert_eq!(
        result,
        BTreeMap::from([(0, d(6)), (1, d(8)), (2, d(-4)), (3, d(5))])
    );
    assert_eq!(
        hierarchical_rollup(&tree, &0, &values, Aggregation::Min, BUDGET).unwrap()[&0],
        d(-4)
    );
    assert_eq!(
        hierarchical_rollup(&tree, &0, &values, Aggregation::Max, BUDGET).unwrap()[&0],
        d(5)
    );
    assert_eq!(
        hierarchical_rollup(
            &graph(),
            &"a",
            &BTreeMap::from([
                ("a", d(1)),
                ("b", d(1)),
                ("c", d(1)),
                ("d", d(1)),
                ("isolated", d(1))
            ]),
            Aggregation::Sum,
            BUDGET
        ),
        Err(Error::NotTree)
    );
}

#[test]
fn tree_rejects_disconnected_nodes_parallel_edges_and_wrong_root() {
    let values = BTreeMap::from([(0, d(1)), (1, d(1))]);
    for edges in [vec![], vec![(0, 1), (0, 1)], vec![(1, 0)]] {
        let tree = Graph {
            nodes: vec![0, 1],
            edges,
        };
        assert_eq!(
            hierarchical_rollup(&tree, &0, &values, Aggregation::Sum, BUDGET),
            Err(Error::NotTree)
        );
    }
}

#[test]
fn arithmetic_never_silently_rounds_or_overflows() {
    assert_eq!(
        Quantity::add(&Decimal::MAX, &d(1)),
        Err(ArithmeticError::Overflow)
    );
    assert_eq!(
        Quantity::add(&Decimal::MAX, &Decimal::new(1, 28)),
        Err(ArithmeticError::Overflow)
    );
    assert_eq!(
        Quantity::mul(&Decimal::new(1, 28), &Decimal::new(1, 1)),
        Err(ArithmeticError::Precision)
    );
    assert_eq!(
        Quantity::mul(&Decimal::new(5, 28), &Decimal::new(2, 1)),
        Ok(Decimal::new(1, 28))
    );
    assert_eq!(Quantity::mul(&Decimal::MAX, &d(0)), Ok(d(0)));
    let graph = Graph {
        nodes: vec![0, 1],
        edges: vec![(0, 1)],
    };
    assert_eq!(
        critical_path(
            &graph,
            &BTreeMap::from([(0, Decimal::MAX), (1, d(1))]),
            BUDGET
        ),
        Err(Error::Arithmetic(ArithmeticError::Overflow))
    );
}

#[test]
fn weighted_algorithms_validate_all_attributes() {
    let graph = Graph {
        nodes: vec![0],
        edges: vec![],
    };
    assert_eq!(
        critical_path(&graph, &BTreeMap::<_, Decimal>::new(), BUDGET),
        Err(Error::MissingValue(0))
    );
    assert_eq!(
        critical_path(&graph, &BTreeMap::from([(0, d(-1))]), BUDGET),
        Err(Error::NegativeValue(0))
    );
    assert_eq!(
        critical_path(&graph, &BTreeMap::from([(0, d(1)), (1, d(1))]), BUDGET),
        Err(Error::UnknownNode(1))
    );
    let mut bom = bom();
    bom.lines[0].quantity = d(-1);
    assert_eq!(
        explode(&bom, &BTreeMap::new(), BUDGET),
        Err(Error::NegativeValue("assembly"))
    );
    bom.lines[0].quantity = d(1);
    bom.parts.get_mut("base").unwrap().lead_time = d(-1);
    assert_eq!(
        explode(&bom, &BTreeMap::new(), BUDGET),
        Err(Error::NegativeValue("base"))
    );
}

#[test]
fn budgets_refuse_before_work_and_have_exact_step_boundary() {
    let graph = graph();
    for (budget, resource, limit) in [
        (
            Budget {
                max_nodes: 4,
                ..BUDGET
            },
            Limit::Nodes,
            4,
        ),
        (
            Budget {
                max_edges: 3,
                ..BUDGET
            },
            Limit::Edges,
            3,
        ),
        (
            Budget {
                max_steps: 0,
                ..BUDGET
            },
            Limit::Steps,
            0,
        ),
    ] {
        assert_eq!(
            topological_order(&graph, budget),
            Err(Error::BudgetExceeded(BudgetExceeded { resource, limit }))
        );
    }
    let graph = Graph {
        nodes: vec![1],
        edges: vec![],
    };
    assert_eq!(
        topological_order(
            &graph,
            Budget {
                max_steps: 3,
                ..BUDGET
            }
        )
        .unwrap(),
        vec![1]
    );
    assert_eq!(
        topological_order(
            &graph,
            Budget {
                max_steps: 2,
                ..BUDGET
            }
        ),
        Err(Error::BudgetExceeded(BudgetExceeded {
            resource: Limit::Steps,
            limit: 2
        }))
    );
}

#[test]
fn all_algorithms_share_the_explicit_meter_including_cycle_explanation() {
    let budget = Budget {
        max_steps: 0,
        ..BUDGET
    };
    let refusal = Error::BudgetExceeded(BudgetExceeded {
        resource: Limit::Steps,
        limit: 0,
    });
    let graph = Graph {
        nodes: vec![0],
        edges: vec![],
    };
    let values = BTreeMap::from([(0, d(1))]);
    assert_eq!(
        reachability(&graph, &[0], Direction::Upstream, budget),
        Err(refusal.clone())
    );
    assert_eq!(critical_path(&graph, &values, budget), Err(refusal.clone()));
    assert_eq!(
        hierarchical_rollup(&graph, &0, &values, Aggregation::Sum, budget),
        Err(refusal)
    );
    let bom = Bom {
        parts: BTreeMap::from([(
            0,
            Part {
                cost: d(1),
                lead_time: d(1),
            },
        )]),
        lines: vec![],
    };
    assert_eq!(
        explode(&bom, &values, budget),
        Err(Error::BudgetExceeded(BudgetExceeded {
            resource: Limit::Steps,
            limit: 0
        }))
    );
    let cycle = Graph {
        nodes: vec![0],
        edges: vec![(0, 0)],
    };
    assert_eq!(
        topological_order(
            &cycle,
            Budget {
                max_steps: 6,
                ..BUDGET
            }
        ),
        Err(Error::BudgetExceeded(BudgetExceeded {
            resource: Limit::Steps,
            limit: 6
        }))
    );
    assert_eq!(
        topological_order(
            &cycle,
            Budget {
                max_steps: 7,
                ..BUDGET
            }
        ),
        Err(Error::Cycle(vec![0, 0]))
    );
}

#[test]
fn empty_graphs_are_well_defined() {
    let graph = Graph::<u8> {
        nodes: vec![],
        edges: vec![],
    };
    let zero_budget = Budget {
        max_nodes: 0,
        max_edges: 0,
        max_steps: 0,
    };
    assert_eq!(topological_order(&graph, zero_budget), Ok(vec![]));
    assert!(
        reachability(&graph, &[], Direction::Downstream, zero_budget)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        critical_path(&graph, &BTreeMap::<_, Decimal>::new(), zero_budget)
            .unwrap()
            .length,
        d(0)
    );
    assert_eq!(
        explode(
            &Bom::<u8, Decimal> {
                parts: BTreeMap::new(),
                lines: vec![]
            },
            &BTreeMap::new(),
            zero_budget
        )
        .unwrap()
        .total_cost,
        d(0)
    );
}

#[test]
fn deep_graphs_use_iterative_traversal() {
    let mut graph = Graph {
        nodes: (0..10_000).collect(),
        edges: (0..9_999).map(|n| (n, n + 1)).collect(),
    };
    assert_eq!(topological_order(&graph, BUDGET).unwrap().len(), 10_000);
    graph.edges.push((9_999, 0));
    let Err(Error::Cycle(cycle)) = topological_order(&graph, BUDGET) else {
        unreachable!()
    };
    assert_eq!(cycle.len(), 10_001);
}

prop_compose! {
    fn dag()(count in 1usize..8)(
        raw in prop::collection::vec((0..count, 0..count, 1i64..5), 0..12),
        weights in prop::collection::vec(0i64..10, count),
        labels in Just((0..count).collect::<Vec<_>>()).prop_shuffle(),
    ) -> (Graph<usize>, Vec<(usize, usize, i64)>, BTreeMap<usize, Decimal>) {
        // Orient edges by latent rank, then independently permute their labels.
        // Rank edge 0 -> 1 with labels [1, 0] becomes 1 -> 0, defeating sorted IDs.
        let lines: Vec<_> = raw.into_iter().filter(|(a,b,_)| a != b).map(|(a,b,q)| (labels[a.min(b)],labels[a.max(b)],q)).collect();
        let graph = Graph { nodes: labels.clone(), edges: lines.iter().map(|(a,b,_)| (*a,*b)).collect() };
        let durations = weights.into_iter().enumerate().map(|(n,w)| (labels[n],d(w))).collect();
        (graph, lines, durations)
    }
}

fn brute_longest(
    node: usize,
    graph: &Graph<usize>,
    durations: &BTreeMap<usize, Decimal>,
) -> Decimal {
    let suffix = graph
        .edges
        .iter()
        .filter(|(a, _)| *a == node)
        .map(|(_, b)| brute_longest(*b, graph, durations))
        .max()
        .unwrap_or(Decimal::ZERO);
    durations[&node] + suffix
}

fn brute_explode(
    node: usize,
    amount: i64,
    lines: &[(usize, usize, i64)],
    totals: &mut BTreeMap<usize, i64>,
) {
    let mut children = lines.iter().filter(|(a, _, _)| *a == node).peekable();
    if children.peek().is_none() {
        *totals.entry(node).or_default() += amount;
    } else {
        for (_, child, quantity) in children {
            brute_explode(*child, amount * quantity, lines, totals);
        }
    }
}

proptest! {
    #[test]
    fn topo_respects_every_edge((graph, _, _) in dag()) {
        let order = topological_order(&graph,BUDGET).unwrap();
        let positions: BTreeMap<_,_> = order.iter().enumerate().map(|(p,n)| (*n,p)).collect();
        prop_assert_eq!(order.len(),graph.nodes.len());
        for (a,b) in graph.edges {
            prop_assert!(positions[&a] < positions[&b]);
        }
    }

    #[test]
    fn critical_length_equals_independent_longest_path((graph, _, durations) in dag()) {
        let expected = graph.nodes.iter().map(|n| brute_longest(*n,&graph,&durations)).max().unwrap_or(Decimal::ZERO);
        let result = critical_path(&graph,&durations,BUDGET).unwrap();
        prop_assert_eq!(result.length,expected);
        for (node,timing) in &result.timings {
            prop_assert!(timing.slack >= Decimal::ZERO);
            prop_assert_eq!(timing.earliest_finish,timing.earliest_start + durations[node]);
            prop_assert_eq!(timing.latest_finish,timing.latest_start + durations[node]);
            prop_assert_eq!(timing.slack,timing.latest_start - timing.earliest_start);
        }
        for (a,b) in &graph.edges {
            prop_assert!(result.timings[b].earliest_start >= result.timings[a].earliest_finish);
            prop_assert!(result.timings[b].latest_start >= result.timings[a].latest_finish);
        }
    }

    #[test]
    fn explosion_matches_brute_force_fold((graph, lines, _) in dag(), demand in 1i64..5) {
        let parts = graph.nodes.iter().map(|n| (*n,Part { cost:d(1),lead_time:d(1) })).collect();
        let bom = Bom { parts, lines:lines.iter().map(|(a,b,q)| Composition { parent:*a, child:*b, quantity:d(*q) }).collect() };
        let result = explode(&bom,&BTreeMap::from([(0,d(demand))]),BUDGET).unwrap();
        let mut expected = BTreeMap::new();
        brute_explode(0,demand,&lines,&mut expected);
        let expected: BTreeMap<_,_> = expected.into_iter().map(|(n,q)| (n,d(q))).collect();
        prop_assert_eq!(result.requirements,expected);
    }

    #[test]
    fn critical_path_has_exact_nonzero_budget_boundary((graph, _, durations) in dag()) {
        let nodes = graph.nodes.len();
        let edges = graph.edges.len();
        // Count work from input cardinalities, without probing the algorithm's meter:
        // preparation, attribute validation, degree/ready processing, forward/backward timing.
        let preparation = nodes + edges;
        let validation = durations.len() + nodes;
        let ordering = 2 * nodes + edges;
        let timing = 2 * (nodes + edges);
        let work = preparation + validation + ordering + timing;
        let boundary = work - 1;
        prop_assert!(boundary > preparation);
        prop_assert_eq!(
            critical_path(&graph, &durations, Budget { max_steps: boundary, ..BUDGET }),
            Err(Error::BudgetExceeded(BudgetExceeded { resource: Limit::Steps, limit: boundary }))
        );
        let result = critical_path(&graph, &durations, Budget { max_steps: boundary + 1, ..BUDGET }).unwrap();
        let expected = graph.nodes.iter().map(|n| brute_longest(*n, &graph, &durations)).max().unwrap();
        prop_assert_eq!(result.length, expected);
        prop_assert_eq!(result.timings.len(), nodes);
    }

    #[test]
    fn bom_propagation_has_exact_nonzero_budget_boundary((graph, lines, _) in dag()) {
        let bom = Bom {
            parts: graph.nodes.iter().map(|n| (*n, Part { cost: d(1), lead_time: d(1) })).collect(),
            lines: lines.iter().map(|(a,b,q)| Composition { parent: *a, child: *b, quantity: d(*q) }).collect(),
        };
        // Positive demand at every part ensures propagation visits every supplied line,
        // including parallel lines and lines in disconnected components.
        let demand: BTreeMap<_,_> = graph.nodes.iter().map(|n| (*n, d(1))).collect();
        let nodes = bom.parts.len();
        let edges = bom.lines.len();
        let preparation = nodes + edges;
        let validation = nodes + edges + demand.len();
        let ordering = 2 * nodes + edges;
        let unit_rollups = nodes + edges;
        let demand_totals = demand.len();
        let propagation = nodes + edges;
        let work = preparation + validation + ordering + unit_rollups + demand_totals + propagation;
        let boundary = work - 1;
        prop_assert!(boundary > preparation);
        prop_assert_eq!(
            explode(&bom, &demand, Budget { max_steps: boundary, ..BUDGET }),
            Err(Error::BudgetExceeded(BudgetExceeded { resource: Limit::Steps, limit: boundary }))
        );
        let result = explode(&bom, &demand, Budget { max_steps: boundary + 1, ..BUDGET }).unwrap();
        let mut expected = BTreeMap::new();
        for node in &graph.nodes {
            brute_explode(*node, 1, &lines, &mut expected);
        }
        let expected: BTreeMap<_,_> = expected.into_iter().map(|(n,q)| (n, d(q))).collect();
        prop_assert_eq!(result.requirements, expected);
        prop_assert_eq!(result.unit_rollups.len(), nodes);
    }

    #[test]
    fn tree_rollup_has_exact_nonzero_budget_boundary((graph, _, values) in dag()) {
        let tree = Graph {
            nodes: graph.nodes.clone(),
            edges: graph.nodes.iter().filter(|n| **n != 0).map(|n| (0, *n)).collect(),
        };
        let nodes = tree.nodes.len();
        let edges = tree.edges.len();
        let preparation = nodes + edges;
        let validation = values.len() + nodes;
        let ordering = 2 * nodes + edges;
        let tree_validation = nodes;
        let rollup = nodes + edges;
        let work = preparation + validation + ordering + tree_validation + rollup;
        let boundary = work - 1;
        prop_assert!(boundary > preparation);
        for aggregation in [Aggregation::Sum, Aggregation::Min, Aggregation::Max] {
            prop_assert_eq!(
                hierarchical_rollup(&tree, &0, &values, aggregation, Budget { max_steps: boundary, ..BUDGET }),
                Err(Error::BudgetExceeded(BudgetExceeded { resource: Limit::Steps, limit: boundary }))
            );
            let result = hierarchical_rollup(&tree, &0, &values, aggregation, Budget { max_steps: boundary + 1, ..BUDGET }).unwrap();
            let expected = match aggregation {
                Aggregation::Sum => values.values().copied().sum(),
                Aggregation::Min => *values.values().min().unwrap(),
                Aggregation::Max => *values.values().max().unwrap(),
            };
            prop_assert_eq!(result[&0], expected);
            prop_assert_eq!(result.len(), nodes);
            for node in tree.nodes.iter().filter(|n| **n != 0) {
                prop_assert_eq!(result[node], values[node]);
            }
        }
    }

    #[test]
    fn exhaustion_is_deterministic_for_every_algorithm((graph, lines, durations) in dag(), steps in 0usize..200) {
        let budget = Budget { max_steps:steps,..BUDGET };
        prop_assert_eq!(topological_order(&graph,budget),topological_order(&graph,budget));
        prop_assert_eq!(reachability(&graph,&[0],Direction::Downstream,budget),reachability(&graph,&[0],Direction::Downstream,budget));
        prop_assert_eq!(critical_path(&graph,&durations,budget),critical_path(&graph,&durations,budget));
        let bom = Bom { parts:graph.nodes.iter().map(|n| (*n,Part { cost:d(1),lead_time:d(1) })).collect(), lines:lines.iter().map(|(a,b,q)| Composition { parent:*a, child:*b, quantity:d(*q) }).collect() };
        let demand = BTreeMap::from([(0,d(1))]);
        prop_assert_eq!(explode(&bom,&demand,budget),explode(&bom,&demand,budget));
        let tree = Graph { nodes:graph.nodes.clone(), edges:graph.nodes.iter().filter(|n| **n != 0).map(|n| (0,*n)).collect() };
        prop_assert_eq!(hierarchical_rollup(&tree,&0,&durations,Aggregation::Sum,budget),hierarchical_rollup(&tree,&0,&durations,Aggregation::Sum,budget));
        if steps == 0 {
            prop_assert!(matches!(topological_order(&graph,budget),Err(Error::BudgetExceeded(_))));
        }
    }
}
