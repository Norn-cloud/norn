//! The executor against independent oracles: reachability computed by plain
//! graph search, and the set identities of stratified negation.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "bounded executor fixtures fail fast"
)]

use std::collections::BTreeSet;

use norn_datalog::{
    Atom, Budget, Code, ExecBudget, Facts, Literal, Mode, Predicate, Program, RelationKind, Rule,
    Term, Tuple, Value, Version, evaluate,
};
use proptest::prelude::*;

fn variable(name: &str) -> Term {
    Term::Variable(name.to_owned())
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn atom(predicate: &str, terms: Vec<Term>) -> Atom {
    Atom {
        predicate: predicate.to_owned(),
        terms,
    }
}

fn over(predicate: &str, variables: &[&str]) -> Atom {
    atom(
        predicate,
        variables.iter().map(|name| variable(name)).collect(),
    )
}

fn rule(id: &str, head: Atom, body: Vec<Literal>) -> Rule {
    Rule {
        id: id.to_owned(),
        mode: Mode::Entailment,
        head,
        body,
    }
}

fn predicate(name: &str, arity: usize, kind: RelationKind) -> Predicate {
    Predicate {
        name: name.to_owned(),
        arity,
        kind,
    }
}

/// `path` is the transitive closure of `edge`; `isolated` is every node with
/// no path from a `start` node, which needs `reached` complete first.
fn reachability() -> Program {
    Program {
        version: Version::V1,
        predicates: vec![
            predicate("edge", 2, RelationKind::Input),
            predicate("node", 1, RelationKind::Input),
            predicate("start", 1, RelationKind::Input),
            predicate("path", 2, RelationKind::Derived),
            predicate("reached", 1, RelationKind::Derived),
            predicate("isolated", 1, RelationKind::Derived),
        ],
        rules: vec![
            rule(
                "path.edge",
                over("path", &["a", "b"]),
                vec![Literal::Positive(over("edge", &["a", "b"]))],
            ),
            rule(
                "path.step",
                over("path", &["a", "c"]),
                vec![
                    Literal::Positive(over("path", &["a", "b"])),
                    Literal::Positive(over("edge", &["b", "c"])),
                ],
            ),
            rule(
                "reached.start",
                over("reached", &["a"]),
                vec![Literal::Positive(over("start", &["a"]))],
            ),
            rule(
                "reached.path",
                over("reached", &["b"]),
                vec![
                    Literal::Positive(over("start", &["a"])),
                    Literal::Positive(over("path", &["a", "b"])),
                ],
            ),
            rule(
                "isolated",
                over("isolated", &["a"]),
                vec![
                    Literal::Negative(over("reached", &["a"])),
                    Literal::Positive(over("node", &["a"])),
                ],
            ),
        ],
    }
}

fn name(node: usize) -> String {
    format!("n{node:02}")
}

fn graph_facts(nodes: usize, edges: &[(usize, usize)], starts: &[usize]) -> Facts {
    Facts::from([
        (
            "edge".to_owned(),
            edges
                .iter()
                .map(|(from, to)| vec![text(&name(*from)), text(&name(*to))])
                .collect(),
        ),
        (
            "node".to_owned(),
            (0..nodes).map(|node| vec![text(&name(node))]).collect(),
        ),
        (
            "start".to_owned(),
            starts.iter().map(|node| vec![text(&name(*node))]).collect(),
        ),
    ])
}

/// Every `(from, to)` joined by a path of at least one edge, by search.
fn closure(nodes: usize, edges: &[(usize, usize)]) -> BTreeSet<(usize, usize)> {
    let mut paths = BTreeSet::new();
    for from in 0..nodes {
        let mut frontier = vec![from];
        while let Some(at) = frontier.pop() {
            for (_, to) in edges.iter().filter(|(source, _)| *source == at) {
                if paths.insert((from, *to)) {
                    frontier.push(*to);
                }
            }
        }
    }
    paths
}

fn run(program: &Program, facts: &Facts) -> norn_datalog::Evaluation {
    evaluate(program, facts, Budget::default(), ExecBudget::default()).expect("evaluates")
}

fn refused(program: &Program, facts: &Facts, budget: ExecBudget) -> norn_datalog::Diagnostic {
    evaluate(program, facts, Budget::default(), budget).expect_err("must be refused")
}

#[test]
fn a_cycle_closes_and_negation_reads_the_finished_relation() {
    // 0 -> 1 -> 2 -> 0 is a cycle; 3 -> 4 hangs off nothing; 5 is alone.
    let facts = graph_facts(6, &[(0, 1), (1, 2), (2, 0), (3, 4)], &[1]);
    let result = run(&reachability(), &facts);
    let path = |from: usize, to: usize| vec![text(&name(from)), text(&name(to))];
    assert_eq!(
        result.tuples("path").to_vec(),
        vec![
            path(0, 0),
            path(0, 1),
            path(0, 2),
            path(1, 0),
            path(1, 1),
            path(1, 2),
            path(2, 0),
            path(2, 1),
            path(2, 2),
            path(3, 4),
        ]
    );
    assert_eq!(
        result.tuples("isolated").to_vec(),
        vec![
            vec![text(&name(3))],
            vec![text(&name(4))],
            vec![text(&name(5))]
        ]
    );
    let names: Vec<&str> = result
        .relations
        .iter()
        .map(|relation| relation.predicate.as_str())
        .collect();
    assert_eq!(names, ["isolated", "path", "reached"]);
    assert_eq!(result.usage.derived_facts, 10 + 3 + 3);
}

#[test]
fn constants_seeds_and_every_value_kind_take_part_in_joins() {
    let scope = Value::Scope(norn_ir::ScopeRef {
        org_id: Some("org".to_owned()),
        ..norn_ir::ScopeRef::default()
    });
    let program = Program {
        version: Version::V1,
        predicates: vec![
            predicate("item", 3, RelationKind::Input),
            predicate("flagged", 2, RelationKind::Derived),
            predicate("root", 1, RelationKind::Derived),
            predicate("rooted", 1, RelationKind::Derived),
        ],
        rules: vec![
            // Only rows whose flag is the constant `true`.
            rule(
                "flagged",
                over("flagged", &["scope", "id"]),
                vec![Literal::Positive(atom(
                    "item",
                    vec![
                        variable("scope"),
                        variable("id"),
                        Term::Constant(Value::Bool(true)),
                    ],
                ))],
            ),
            // A ground clause with no body is a seed fact.
            rule(
                "root.seed",
                atom("root", vec![Term::Constant(text("b"))]),
                vec![],
            ),
            rule(
                "rooted",
                over("rooted", &["id"]),
                vec![
                    Literal::Positive(over("root", &["id"])),
                    Literal::Positive(over("flagged", &["scope", "id"])),
                ],
            ),
        ],
    };
    let facts = Facts::from([(
        "item".to_owned(),
        vec![
            vec![scope.clone(), text("a"), Value::Bool(true)],
            vec![scope.clone(), text("b"), Value::Bool(true)],
            vec![scope.clone(), text("c"), Value::Bool(false)],
            // A repeated tuple is one fact.
            vec![scope.clone(), text("a"), Value::Bool(true)],
        ],
    )]);
    let result = run(&program, &facts);
    assert_eq!(
        result.tuples("flagged").to_vec(),
        vec![vec![scope.clone(), text("a")], vec![scope, text("b")]]
    );
    assert_eq!(result.tuples("root").to_vec(), vec![vec![text("b")]]);
    assert_eq!(result.tuples("rooted").to_vec(), vec![vec![text("b")]]);
    assert_eq!(result.usage.input_facts, 4);
}

#[test]
fn the_overlap_pack_runs_as_data() {
    let scope = Value::Scope(norn_ir::ScopeRef::default());
    let link = |owner: &str, to: &str, required: bool| {
        vec![
            scope.clone(),
            text(owner),
            text("lines"),
            text("Line"),
            text(to),
            text("one"),
            Value::Bool(required),
            Value::Bool(false),
        ]
    };
    let facts = Facts::from([
        (
            "extends".to_owned(),
            vec![vec![scope.clone(), text("Child"), text("Parent")]],
        ),
        (
            "link".to_owned(),
            vec![link("Parent", "one", true), link("Child", "many", false)],
        ),
        (
            "field".to_owned(),
            vec![vec![
                scope.clone(),
                text("Child"),
                text("lines"),
                text("text"),
                Value::Bool(false),
                Value::Bool(false),
            ]],
        ),
    ]);
    let result = run(&norn_datalog::overlap::declared_metadata_v1(), &facts);
    let widened =
        |dimension: &str| vec![scope.clone(), text("Child"), text("lines"), text(dimension)];
    assert_eq!(
        result.tuples("cardinality_widening").to_vec(),
        vec![widened("required"), widened("to")]
    );
    assert_eq!(
        result.tuples("member_collision").to_vec(),
        vec![vec![scope.clone(), text("Child"), text("lines")]]
    );
    assert!(result.tuples("inheritance_cycle").is_empty());
}

#[test]
fn an_exhausted_budget_refuses_and_names_the_rule_that_was_running() {
    let chain: Vec<(usize, usize)> = (0..40).map(|node| (node, node + 1)).collect();
    let facts = graph_facts(41, &chain, &[0]);
    let program = reachability();
    // 41 nodes in a chain have 820 paths.
    assert_eq!(run(&program, &facts).tuples("path").len(), 820);
    // The meter is exact: the work a run reports is enough, one unit less is not.
    let needed = run(&program, &facts).usage.work;
    let exact = ExecBudget {
        max_work: needed,
        ..ExecBudget::default()
    };
    assert!(evaluate(&program, &facts, Budget::default(), exact).is_ok());
    let work = refused(
        &program,
        &facts,
        ExecBudget {
            max_work: needed - 1,
            ..ExecBudget::default()
        },
    );
    assert_eq!(
        (work.code, work.rule_id.as_deref()),
        (Code::BudgetExhausted, Some("isolated"))
    );
    let derived = refused(
        &program,
        &facts,
        ExecBudget {
            // Round one derives 40 paths and 1 reached node; the 20th
            // two-edge path is the 61st fact.
            max_derived_facts: 60,
            ..ExecBudget::default()
        },
    );
    assert_eq!(
        (derived.code, derived.rule_id.as_deref()),
        (Code::BudgetExhausted, Some("path.step"))
    );
    let input = refused(
        &program,
        &facts,
        ExecBudget {
            max_input_facts: 81,
            ..ExecBudget::default()
        },
    );
    assert_eq!(input.code, Code::BudgetExhausted);
    let bytes = refused(
        &program,
        &facts,
        ExecBudget {
            max_text_bytes: 100,
            ..ExecBudget::default()
        },
    );
    assert_eq!(bytes.code, Code::BudgetExhausted);
}

#[test]
fn facts_that_do_not_fit_the_declared_signatures_are_refused() {
    let program = reachability();
    let with = |relation: &str, tuple: Tuple| {
        let mut facts = graph_facts(2, &[(0, 1)], &[0]);
        facts.entry(relation.to_owned()).or_default().push(tuple);
        refused(&program, &facts, ExecBudget::default())
    };
    let undefined = with("missing", vec![text("x")]);
    assert_eq!(
        (undefined.code, undefined.symbol.as_deref()),
        (Code::UndefinedPredicate, Some("missing"))
    );
    let derived = with("path", vec![text("x"), text("y")]);
    assert_eq!(
        (derived.code, derived.symbol.as_deref()),
        (Code::DerivedInInput, Some("path"))
    );
    assert_eq!(with("edge", vec![text("x")]).code, Code::ArityMismatch);
    // U+0065 U+0301 is not NFC; its composed form is U+00E9.
    assert_eq!(
        with("node", vec![text("e\u{301}")]).code,
        Code::NoncanonicalConstant
    );
}

#[test]
fn a_program_the_contract_refuses_is_never_run() {
    let mut program = reachability();
    // `reached` now depends negatively on `isolated`, which depends on it.
    program.rules.push(rule(
        "reached.loop",
        over("reached", &["a"]),
        vec![
            Literal::Positive(over("node", &["a"])),
            Literal::Negative(over("isolated", &["a"])),
        ],
    ));
    let facts = graph_facts(2, &[(0, 1)], &[0]);
    let diagnostic = refused(&program, &facts, ExecBudget::default());
    assert_eq!(diagnostic.code, Code::RecursionThroughNegation);
    assert_eq!(
        Err(diagnostic),
        norn_datalog::validate(&program, Budget::default()).map(|_plan| ())
    );
}

fn graphs() -> impl Strategy<Value = (usize, Vec<(usize, usize)>, Vec<usize>)> {
    (1_usize..9).prop_flat_map(|nodes| {
        (
            Just(nodes),
            proptest::collection::vec((0..nodes, 0..nodes), 0..24),
            proptest::collection::vec(0..nodes, 0..3),
        )
    })
}

proptest! {
    /// `path` is exactly graph reachability, and `isolated` is exactly the
    /// nodes outside the start set and everything reachable from it.
    #[test]
    fn recursion_and_negation_agree_with_graph_search((nodes, edges, starts) in graphs()) {
        let result = run(&reachability(), &graph_facts(nodes, &edges, &starts));
        let expected = closure(nodes, &edges);
        let paths: Vec<Tuple> = expected
            .iter()
            .map(|(from, to)| vec![text(&name(*from)), text(&name(*to))])
            .collect();
        prop_assert_eq!(result.tuples("path"), paths.as_slice());
        let reached: BTreeSet<usize> = starts
            .iter()
            .copied()
            .chain(expected.iter().filter(|(from, _)| starts.contains(from)).map(|(_, to)| *to))
            .collect();
        let isolated: Vec<Tuple> = (0..nodes)
            .filter(|node| !reached.contains(node))
            .map(|node| vec![text(&name(node))])
            .collect();
        prop_assert_eq!(result.tuples("isolated"), isolated.as_slice());
    }

    /// The result and the metered work depend on the set of facts only: not
    /// on their order, not on repeats. The result does not depend on rule
    /// order either.
    #[test]
    fn the_result_is_independent_of_fact_and_rule_order(
        (nodes, edges, starts) in graphs(),
        rotate in 0_usize..24,
    ) {
        let program = reachability();
        let facts = graph_facts(nodes, &edges, &starts);
        let baseline = run(&program, &facts);
        let mut shuffled = facts.clone();
        for tuples in shuffled.values_mut() {
            if !tuples.is_empty() {
                let at = rotate % tuples.len();
                tuples.rotate_left(at);
                tuples.reverse();
            }
        }
        let reordered = run(&program, &shuffled);
        prop_assert_eq!(&reordered.relations, &baseline.relations);
        prop_assert_eq!(reordered.usage, baseline.usage);
        let mut repeated = facts.clone();
        for tuples in repeated.values_mut() {
            let again = tuples.clone();
            tuples.extend(again);
        }
        prop_assert_eq!(&run(&program, &repeated).relations, &baseline.relations);
        let mut reversed = program;
        reversed.rules.reverse();
        reversed.predicates.reverse();
        prop_assert_eq!(&run(&reversed, &facts).relations, &baseline.relations);
    }
}
