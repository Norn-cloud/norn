//! LIFE-SOUND-4 over the PROOF-LIFECYCLE-3 finite graph: option to complete over exact rows,
//! with the smallest law set that blocks each stuck row's exits, and facts never refused.
//!
//! Every edge of the bounded graph is exact (guards and laws are evaluated on the row), so
//! each one is a must edge and the result is exhaustive for the pinned profile. A graph that
//! could not be built stays Unknown with its reason; nothing here is solver evidence.
use super::graph::Graph;
use crate::lifecycle_soundness::codes;
use crate::soundness::{self, Completion, Ending, Modality, Net, NetEdge};
use norn_ir::{LifecycleOutcome, RuntimeValue, TransitionTrigger};
use std::collections::BTreeSet;

/// One refuted row of a bounded lifecycle graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowFinding {
    /// `lifecycle.soundness.law_blocked_exit`, `.trap` or `.fact_refused`.
    pub code: &'static str,
    /// The row index in the snapshot.
    pub row: usize,
    /// The row's lifecycle state.
    pub state: String,
    /// A shortest lawful run to the row, as transition ids.
    pub run: Vec<String>,
    /// For a blocked exit: the transition, and the smallest set of governing laws whose
    /// refusal blocks it (one exit's whole core; the smallest over the row's exits).
    pub blocked: Option<(String, Vec<String>)>,
}

impl Graph {
    /// Soundness of the bounded relation: every reachable row can reach an ending row.
    /// A stuck row whose exits laws refuse is `law_blocked_exit` with the minimal core; a stuck
    /// row with no enabled exit, or on a cycle that never ends, is `trap`. A reachable fact
    /// transition a law refuses is `fact_refused`.
    /// # Errors
    /// A row without a string lifecycle state.
    #[allow(
        clippy::too_many_lines,
        reason = "one fixpoint, then the two finding kinds over it"
    )]
    pub fn soundness(&self) -> Result<Vec<RowFinding>, String> {
        let chart = &self.inputs.machine.lifecycle;
        let property = chart.property.as_str();
        let states = self
            .inputs
            .nodes
            .iter()
            .map(|row| match row.get(property) {
                Some(RuntimeValue::String(state)) => Ok(state.as_str()),
                _ => Err("missing finite lifecycle state".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ending = |state: &str| match chart
            .endings
            .iter()
            .find(|ending| ending.state == state)
            .map(|ending| ending.outcome)
        {
            Some(LifecycleOutcome::Resting) => Some(Ending::Resting),
            Some(LifecycleOutcome::Compensated) => Some(Ending::Compensated),
            Some(LifecycleOutcome::Cancelled) => Some(Ending::Cancelled),
            Some(LifecycleOutcome::BreachedVisible) => Some(Ending::BreachedVisible),
            Some(LifecycleOutcome::Completed) => Some(Ending::Good),
            None => chart
                .terminal
                .iter()
                .any(|t| t == state)
                .then_some(Ending::Good),
        };
        let timer = |transition: &str| {
            chart.transitions.iter().any(|declared| {
                declared.id.as_str() == transition
                    && matches!(declared.trigger, Some(TransitionTrigger::Timer { .. }))
            })
        };
        let net = Net {
            states: states.len(),
            initial: self.initial.clone(),
            endings: states.iter().map(|state| ending(state)).collect(),
            edges: self
                .edges
                .iter()
                .enumerate()
                .map(|(label, edge)| NetEdge {
                    from: edge.from,
                    to: edge.to,
                    label,
                    modality: Modality::Must,
                    input: !timer(&edge.transition),
                })
                .collect(),
        };
        let verdict = soundness::analyse(&net);
        let run = |row: usize| -> Vec<String> {
            soundness::witness(&net, row)
                .unwrap_or_default()
                .iter()
                .filter_map(|edge| self.edges.get(*edge))
                .map(|edge| edge.transition.clone())
                .collect()
        };
        let stuck: BTreeSet<usize> = verdict
            .states
            .iter()
            .enumerate()
            .filter(|(_, outcome)| outcome.stuck())
            .map(|(row, _)| row)
            .collect();
        let mut forward = vec![Vec::new(); net.states];
        let mut backward = vec![Vec::new(); net.states];
        for edge in &net.edges {
            if let (Some(out), Some(back)) = (forward.get_mut(edge.from), backward.get_mut(edge.to))
            {
                out.push(edge.to);
                back.push(edge.from);
            }
        }
        let mut out = Vec::new();
        for &row in &stuck {
            let Some(outcome) = verdict.states.get(row) else {
                continue;
            };
            let core = self
                .blocked
                .iter()
                .filter(|blocked| blocked.from == row)
                .min_by_key(|blocked| blocked.laws.len());
            // A stuck row with exits that are themselves stuck is reported only where the
            // cause is: a deadlock, or a cycle that never ends (a bottom component).
            let bottom =
                outcome.deadlock || closure(&forward, row).is_subset(&closure(&backward, row));
            if core.is_none() && !bottom {
                continue;
            }
            out.push(RowFinding {
                code: if core.is_some() {
                    codes::LAW_BLOCKED_EXIT
                } else {
                    codes::TRAP
                },
                row,
                state: states.get(row).copied().unwrap_or_default().to_owned(),
                run: run(row),
                blocked: core.map(|blocked| (blocked.transition.clone(), blocked.laws.clone())),
            });
        }
        for blocked in &self.blocked {
            let fact = chart
                .transitions
                .iter()
                .any(|declared| declared.id.as_str() == blocked.transition && declared.is_fact());
            if fact
                && verdict
                    .states
                    .get(blocked.from)
                    .is_some_and(|outcome| outcome.reachable)
            {
                out.push(RowFinding {
                    code: codes::FACT_REFUSED,
                    row: blocked.from,
                    state: states
                        .get(blocked.from)
                        .copied()
                        .unwrap_or_default()
                        .to_owned(),
                    run: run(blocked.from),
                    blocked: Some((blocked.transition.clone(), blocked.laws.clone())),
                });
            }
        }
        debug_assert!(
            out.iter()
                .all(|finding| verdict
                    .states
                    .get(finding.row)
                    .is_some_and(|outcome| outcome.reachable
                        && (finding.code == codes::FACT_REFUSED
                            || outcome.completion != Completion::Must
                            || outcome.deadlock))),
            "only reachable stuck rows are refuted"
        );
        Ok(out)
    }
}

/// Rows reachable from `row` along `next`, `row` included.
fn closure(next: &[Vec<usize>], row: usize) -> BTreeSet<usize> {
    let mut seen = BTreeSet::from([row]);
    let mut frontier = vec![row];
    while let Some(current) = frontier.pop() {
        for &to in next.get(current).into_iter().flatten() {
            if seen.insert(to) {
                frontier.push(to);
            }
        }
    }
    seen
}
