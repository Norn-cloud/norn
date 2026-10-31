//! Exhaustive duty separation over a complete finite action graph. This is
//! model evidence, never an authorization decision or a temporal proof.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// An action edge in the effective, single-object lifecycle graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DutyEdge {
    /// Exact ActionIR identity.
    pub action: String,
    /// State indices in the supplied graph.
    pub from: usize,
    /// Destination state index; fact-producing actions may be self edges.
    pub to: usize,
}

/// Complete finite projection supplied by the definition adapter. Opaque effects
/// or incomplete grants must be refused by that adapter, never omitted here.
pub struct DutyGraph {
    /// Effective lifecycle states, including any admitted fact-presence axes.
    pub states: Vec<String>,
    /// States in which a record can be created.
    pub initial: BTreeSet<usize>,
    /// Every action capable of affecting the declared sensitive path.
    pub edges: Vec<DutyEdge>,
    /// Complete effective role grants, by action identity.
    pub grants: BTreeMap<String, BTreeSet<String>>,
}

/// One step of an inspectable path. Prefix steps can belong to other roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DutyStep {
    /// The actual graph edge.
    pub edge: DutyEdge,
    /// A role with the declared grant for this edge.
    pub role: String,
}

/// Evidence is relative to the supplied graph. Guards erased by an adapter
/// make a counterexample only possible, and cannot witness live reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DutyCheck {
    /// No declared role can execute the sequence, after any reachable prefix.
    Separated {
        /// Number of distinct roles exhaustively checked (singletons only).
        roles_checked: usize,
    },
    /// The role executes the entire continuation after the unrestricted prefix.
    Counterexample {
        /// Role capable of the sensitive continuation.
        role: String,
        /// Prefix followed by the violating continuation.
        trace: Vec<DutyStep>,
        /// Index of the first sensitive action in the trace.
        sensitive_start: usize,
    },
    /// Even the unrestricted graph cannot complete the declared sequence.
    Unreachable,
    /// Malformed or incomplete graph/declaration; no successful check occurred.
    Invalid,
    /// Search did not finish within the caller's whole-check budget.
    Exhausted,
}

type Node = (usize, usize);
type Parents = BTreeMap<Node, Option<(Node, DutyStep)>>;

/// Check every singleton role. Sequence progress survives intervening actions
/// and repeats on the same object. Unrestricted prefixes are searched separately
/// so a sales-only create/submit cannot conceal an approver's excessive grants.
/// `budget` meters every expanded edge over all searches, including prefixes.
#[must_use]
pub fn check_duties(graph: &DutyGraph, sequence: &[String], budget: usize) -> DutyCheck {
    if graph.initial.is_empty()
        || sequence.is_empty()
        || graph.initial.iter().any(|s| *s >= graph.states.len())
        || graph.edges.iter().any(|e| {
            e.from >= graph.states.len()
                || e.to >= graph.states.len()
                || e.action.trim().is_empty()
                || !graph.grants.contains_key(&e.action)
        })
        || sequence
            .iter()
            .any(|action| !graph.edges.iter().any(|e| &e.action == action))
        || graph
            .grants
            .values()
            .flatten()
            .any(|role| role.trim().is_empty())
    {
        return DutyCheck::Invalid;
    }
    let mut remaining = budget;
    let roles: BTreeSet<_> = graph.grants.values().flatten().cloned().collect();
    // An unrestricted search checks structural non-vacuity. Every node is
    // finite (state × sequence progress); guards remain the adapter's concern.
    let unrestricted = match search(graph, sequence, None, &mut remaining) {
        Ok((_, end)) => end,
        Err(()) => return DutyCheck::Exhausted,
    };
    if unrestricted.is_none() {
        return DutyCheck::Unreachable;
    }
    for role in &roles {
        let (parents, end) = match search(graph, sequence, Some(role), &mut remaining) {
            Ok(result) => result,
            Err(()) => return DutyCheck::Exhausted,
        };
        if let Some(end) = end {
            let (trace, sensitive_start) = trace(&parents, end);
            return DutyCheck::Counterexample {
                role: role.clone(),
                trace,
                sensitive_start,
            };
        }
    }
    DutyCheck::Separated {
        roles_checked: roles.len(),
    }
}

fn search(
    graph: &DutyGraph,
    sequence: &[String],
    role: Option<&String>,
    remaining: &mut usize,
) -> Result<(Parents, Option<Node>), ()> {
    let starts: BTreeSet<_> = graph.initial.iter().map(|state| (*state, 0)).collect();
    let mut parents: Parents = starts.iter().map(|node| (*node, None)).collect();
    let mut queue: VecDeque<_> = starts.into_iter().collect();
    while let Some(node @ (state, progress)) = queue.pop_front() {
        if progress == sequence.len() {
            return Ok((parents, Some(node)));
        }
        for edge in graph.edges.iter().filter(|edge| edge.from == state) {
            *remaining = remaining.checked_sub(1).ok_or(())?;
            let Some(grants) = graph.grants.get(&edge.action) else {
                return Err(());
            };
            // Until this role starts the sensitive sequence, any granted
            // action can be an unrestricted prefix, even a sensitive action.
            if progress == 0 && role.is_some() {
                if let Some(actor) = grants.first() {
                    insert(&mut parents, &mut queue, node, (edge.to, 0), edge, actor);
                }
            }
            let actor = match role {
                Some(role) if grants.contains(role) => role,
                Some(_) => continue,
                None => match grants.first() {
                    Some(role) => role,
                    None => continue,
                },
            };
            let next = (
                edge.to,
                progress + usize::from(sequence.get(progress) == Some(&edge.action)),
            );
            insert(&mut parents, &mut queue, node, next, edge, actor);
        }
    }
    Ok((parents, None))
}

fn insert(
    parents: &mut Parents,
    queue: &mut VecDeque<Node>,
    previous: Node,
    next: Node,
    edge: &DutyEdge,
    actor: &str,
) {
    if let std::collections::btree_map::Entry::Vacant(entry) = parents.entry(next) {
        entry.insert(Some((
            previous,
            DutyStep {
                edge: edge.clone(),
                role: actor.to_owned(),
            },
        )));
        queue.push_back(next);
    }
}

fn trace(parents: &Parents, mut end: Node) -> (Vec<DutyStep>, usize) {
    let mut steps = Vec::new();
    let mut start_from_end = 0;
    while let Some(Some((previous, step))) = parents.get(&end) {
        if previous.1 == 0 && end.1 == 1 {
            start_from_end = steps.len();
        }
        steps.push(step.clone());
        end = *previous;
    }
    let sensitive_start = steps.len() - start_from_end - 1;
    steps.reverse();
    (steps, sensitive_start)
}
