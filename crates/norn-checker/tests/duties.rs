//! `SalesOrder` duty separation, including unrestricted creation/submission.
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
            to: from.saturating_add(1),
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
        ids.iter().skip(2).cloned().collect(),
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
fn sales_only_prefix_cannot_hide_an_overprivileged_approver() -> Result<(), String> {
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
        return Err(format!("expected counterexample: {result:?}"));
    };
    assert_eq!(role, "approver");
    assert_eq!(sensitive_start, 2);
    assert_eq!(trace.len(), 5);
    assert!(trace.iter().take(2).all(|step| step.role == "sales"));
    assert!(trace.iter().skip(2).all(|step| step.role == "approver"));
    assert_eq!(
        trace
            .iter()
            .skip(2)
            .map(|s| &s.edge.action)
            .collect::<Vec<_>>(),
        sequence.iter().collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn intervening_actions_also_require_the_same_role() -> Result<(), String> {
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
    graph.edges.get_mut(3).ok_or("missing invoice edge")?.from = 6;
    assert_eq!(
        check_duties(&graph, &sequence, 1000),
        DutyCheck::Separated { roles_checked: 4 }
    );
    Ok(())
}

#[test]
fn unknown_invalid_and_vacuous_are_never_separated() {
    let (mut graph, sequence) = order();
    assert_eq!(check_duties(&graph, &sequence, 0), DutyCheck::Exhausted);
    graph
        .grants
        .insert("alpha.order.record_payment".into(), BTreeSet::new());
    assert_eq!(
        check_duties(&graph, &sequence, 1000),
        DutyCheck::Unreachable
    );
    graph.grants = BTreeMap::new();
    assert_eq!(check_duties(&graph, &sequence, 1000), DutyCheck::Invalid);
}
