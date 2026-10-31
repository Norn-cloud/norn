//! SalesOrder duty separation, including unrestricted creation/submission.
use norn_checker::duties::{DutyCheck, DutyEdge, DutyGraph, check_duties};
use std::collections::{BTreeMap, BTreeSet};

fn order() -> (DutyGraph, Vec<String>) {
    let actions = [
        "create",
        "submit",
        "approve",
        "record_invoice",
        "record_payment",
    ];
    let roles = ["sales", "sales", "approver", "billing", "treasury"];
    let ids: Vec<String> = actions
        .iter()
        .map(|id| format!("alpha.order.{id}"))
        .collect();
    let edges = ids
        .iter()
        .enumerate()
        .map(|(from, action)| DutyEdge {
            action: action.clone(),
            from,
            to: from + 1,
        })
        .collect();
    let grants = ids
        .iter()
        .zip(roles)
        .map(|(id, role)| (id.clone(), BTreeSet::from([role.to_owned()])))
        .collect();
    (
        DutyGraph {
            states: (0..6).map(|id| id.to_string()).collect(),
            initial: BTreeSet::from([0]),
            edges,
            grants,
        },
        ids[2..].to_vec(),
    )
}

#[test]
fn distinct_roles_separate_the_sensitive_path() {
    let (graph, sequence) = order();
    assert_eq!(
        check_duties(&graph, &sequence, 1000),
        DutyCheck::Separated { roles_checked: 4 }
    );
}

#[test]
fn sales_only_prefix_cannot_hide_an_overprivileged_approver() {
    let (mut graph, sequence) = order();
    for action in &sequence {
        graph
            .grants
            .entry(action.clone())
            .or_default()
            .insert("approver".into());
    }
    let result = check_duties(&graph, &sequence, 1000);
    let DutyCheck::Counterexample {
        role,
        trace,
        sensitive_start,
    } = result
    else {
        panic!("expected counterexample: {result:?}");
    };
    assert_eq!(role, "approver");
    assert_eq!(sensitive_start, 2);
    assert_eq!(trace.len(), 5);
    assert!(trace[..2].iter().all(|step| step.role == "sales"));
    assert!(trace[2..].iter().all(|step| step.role == "approver"));
    assert_eq!(
        trace[2..]
            .iter()
            .map(|s| &s.edge.action)
            .collect::<Vec<_>>(),
        sequence.iter().collect::<Vec<_>>()
    );
}

#[test]
fn intervening_actions_also_require_the_same_role() {
    let (mut graph, sequence) = order();
    for action in &sequence {
        graph
            .grants
            .entry(action.clone())
            .or_default()
            .insert("approver".into());
    }
    graph.states.push("routed".into());
    graph.edges.push(DutyEdge {
        action: "alpha.order.route".into(),
        from: 3,
        to: 6,
    });
    graph.grants.insert(
        "alpha.order.route".into(),
        BTreeSet::from(["billing".into()]),
    );
    graph.edges[3].from = 6;
    assert_eq!(
        check_duties(&graph, &sequence, 1000),
        DutyCheck::Separated { roles_checked: 4 }
    );
}

#[test]
fn unknown_invalid_and_vacuous_are_never_separated() {
    let (mut graph, sequence) = order();
    assert_eq!(check_duties(&graph, &sequence, 0), DutyCheck::Exhausted);
    graph.grants.insert(sequence[2].clone(), BTreeSet::new());
    assert_eq!(
        check_duties(&graph, &sequence, 1000),
        DutyCheck::Unreachable
    );
    graph.grants = BTreeMap::new();
    assert_eq!(check_duties(&graph, &sequence, 1000), DutyCheck::Invalid);
}
