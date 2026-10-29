use std::collections::BTreeMap;

use crate::graph::{Meter, Prepared, prepare};
use crate::{Budget, Error, Graph, Quantity};

/// Earliest/latest timing and float for a node in a precedence DAG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing<Q> {
    /// Earliest start from project time zero.
    pub earliest_start: Q,
    /// Earliest finish, including this node's duration.
    pub earliest_finish: Q,
    /// Latest start preserving the overall project finish.
    pub latest_start: Q,
    /// Latest finish preserving the overall project finish.
    pub latest_finish: Q,
    /// Latest start minus earliest start.
    pub slack: Q,
}

/// Critical-path schedule for the entire DAG (including disconnected components).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CriticalPath<N, Q> {
    /// Maximum sum of durations along a directed path; zero for an empty graph.
    pub length: Q,
    /// Per-node schedule. Every sink shares the global project finish deadline.
    pub timings: BTreeMap<N, Timing<Q>>,
}

fn value<'a, N: Ord + Clone, Q>(values: &'a BTreeMap<N, Q>, node: &N) -> Result<&'a Q, Error<N>> {
    values
        .get(node)
        .ok_or_else(|| Error::MissingValue(node.clone()))
}

fn nonnegative<N: Clone, Q: Quantity>(node: &N, quantity: &Q) -> Result<(), Error<N>> {
    if quantity < &Q::zero() {
        return Err(Error::NegativeValue(node.clone()));
    }
    Ok(())
}

fn validate_values<N: Ord + Clone, Q: Quantity>(
    graph: &Prepared<N>,
    values: &BTreeMap<N, Q>,
    positive: bool,
    meter: &mut Meter,
) -> Result<(), Error<N>> {
    for (node, quantity) in values {
        meter.step()?;
        graph.links(node)?;
        if positive {
            nonnegative(node, quantity)?;
        }
    }
    for node in graph.nodes.keys() {
        meter.step()?;
        value(values, node)?;
    }
    Ok(())
}

/// Compute earliest/latest starts, finishes and slack for nonnegative durations.
///
/// Edges point from prerequisite to successor. Durations must cover exactly the
/// declared nodes. Disconnected components are scheduled against one deadline.
///
/// # Errors
/// Returns cycles, missing/unknown/negative durations, exact arithmetic failures,
/// invalid graph declarations, or budget exhaustion.
pub fn critical_path<N: Ord + Clone, Q: Quantity>(
    graph: &Graph<N>,
    durations: &BTreeMap<N, Q>,
    budget: Budget,
) -> Result<CriticalPath<N, Q>, Error<N>> {
    let mut meter = Meter::new(budget, graph.nodes.len(), graph.edges.len())?;
    let graph = prepare(graph, &mut meter)?;
    validate_values(&graph, durations, true, &mut meter)?;
    let order = graph.order(&mut meter)?;
    let mut timings: BTreeMap<N, Timing<Q>> = BTreeMap::new();
    let mut length = Q::zero();
    for node in &order {
        meter.step()?;
        let mut start = Q::zero();
        for parent in &graph.links(node)?.incoming {
            meter.step()?;
            start = start.max(
                timings
                    .get(parent)
                    .ok_or(Error::Invariant)?
                    .earliest_finish
                    .clone(),
            );
        }
        let finish = start.add(value(durations, node)?)?;
        length = length.max(finish.clone());
        timings.insert(
            node.clone(),
            Timing {
                earliest_start: start,
                earliest_finish: finish,
                latest_start: Q::zero(),
                latest_finish: Q::zero(),
                slack: Q::zero(),
            },
        );
    }
    for node in order.iter().rev() {
        meter.step()?;
        let mut finish = length.clone();
        for child in &graph.links(node)?.out {
            meter.step()?;
            finish = finish.min(
                timings
                    .get(child)
                    .ok_or(Error::Invariant)?
                    .latest_start
                    .clone(),
            );
        }
        let start = finish.sub(value(durations, node)?)?;
        let timing = timings.get_mut(node).ok_or(Error::Invariant)?;
        timing.slack = start.sub(&timing.earliest_start)?;
        timing.latest_start = start;
        timing.latest_finish = finish;
    }
    Ok(CriticalPath { length, timings })
}

/// Per-unit intrinsic cost and duration, or a rolled-up per-unit result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part<Q> {
    /// Intrinsic cost before child contributions (including assembly costs).
    pub cost: Q,
    /// Intrinsic lead time added after the longest required child path.
    pub lead_time: Q,
}

/// A BOM line: one parent unit requires `quantity` units of a child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition<N, Q> {
    /// Assembly/parent identifier.
    pub parent: N,
    /// Component/child identifier.
    pub child: N,
    /// Nonnegative exact quantity per unit of parent.
    pub quantity: Q,
}

/// Bill of materials. Shared subassemblies and parallel lines are supported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bom<N, Q> {
    /// Every part, including base components, with intrinsic cost and lead time.
    pub parts: BTreeMap<N, Part<Q>>,
    /// Directed parent-to-child composition lines.
    pub lines: Vec<Composition<N, Q>>,
}

/// Exact BOM requirements and roll-ups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explosion<N, Q> {
    /// Summed requirements for reached base components (parts with no lines).
    pub requirements: BTreeMap<N, Q>,
    /// Summed cost of the requested quantities, including intrinsic assembly costs.
    pub total_cost: Q,
    /// Maximum lead time among positive requested quantities (not quantity-scaled).
    pub lead_time: Q,
    /// Unit cost and max-path lead time for every declared part.
    pub unit_rollups: BTreeMap<N, Part<Q>>,
}

/// Explode arbitrary part demands into base requirements and cost/lead roll-ups.
///
/// Every supplied part/line is validated, even when outside the demand closure.
/// Cost sums weighted children plus intrinsic cost. Lead time adds intrinsic
/// time to the maximum child time; zero-quantity lines do not contribute.
///
/// # Errors
/// Returns cycles (including zero-quantity lines), undeclared references,
/// negative quantities/costs/times, exact arithmetic failures, or budget exhaustion.
pub fn explode<N: Ord + Clone, Q: Quantity>(
    bom: &Bom<N, Q>,
    demand: &BTreeMap<N, Q>,
    budget: Budget,
) -> Result<Explosion<N, Q>, Error<N>> {
    let mut meter = Meter::new(budget, bom.parts.len(), bom.lines.len())?;
    let graph = Prepared::new(
        bom.parts.keys(),
        bom.lines.iter().map(|line| (&line.parent, &line.child)),
        &mut meter,
    )?;
    let mut lines: BTreeMap<N, Vec<&Composition<N, Q>>> = BTreeMap::new();
    for (node, part) in &bom.parts {
        meter.step()?;
        nonnegative(node, &part.cost)?;
        nonnegative(node, &part.lead_time)?;
    }
    for line in &bom.lines {
        meter.step()?;
        nonnegative(&line.parent, &line.quantity)?;
        lines.entry(line.parent.clone()).or_default().push(line);
    }
    let mut amounts = BTreeMap::new();
    for (node, quantity) in demand {
        meter.step()?;
        graph.links(node)?;
        nonnegative(node, quantity)?;
        amounts.insert(node.clone(), quantity.clone());
    }
    let order = graph.order(&mut meter)?;
    let mut unit_rollups: BTreeMap<N, Part<Q>> = BTreeMap::new();
    for node in order.iter().rev() {
        meter.step()?;
        let mut rolled = bom.parts.get(node).ok_or(Error::Invariant)?.clone();
        let mut child_time = Q::zero();
        if let Some(children) = lines.get(node) {
            for line in children {
                meter.step()?;
                if line.quantity == Q::zero() {
                    continue;
                }
                let child = unit_rollups.get(&line.child).ok_or(Error::Invariant)?;
                rolled.cost = rolled.cost.add(&line.quantity.mul(&child.cost)?)?;
                child_time = child_time.max(child.lead_time.clone());
            }
        }
        rolled.lead_time = rolled.lead_time.add(&child_time)?;
        unit_rollups.insert(node.clone(), rolled);
    }
    let mut total_cost = Q::zero();
    let mut lead_time = Q::zero();
    for (node, quantity) in demand {
        meter.step()?;
        if quantity == &Q::zero() {
            continue;
        }
        let rolled = unit_rollups.get(node).ok_or(Error::Invariant)?;
        total_cost = total_cost.add(&quantity.mul(&rolled.cost)?)?;
        lead_time = lead_time.max(rolled.lead_time.clone());
    }
    let mut requirements = BTreeMap::new();
    for node in &order {
        meter.step()?;
        let Some(amount) = amounts.get(node).cloned() else {
            continue;
        };
        if amount == Q::zero() {
            continue;
        }
        if let Some(children) = lines.get(node) {
            for line in children {
                meter.step()?;
                if line.quantity == Q::zero() {
                    continue;
                }
                let increment = amount.mul(&line.quantity)?;
                let next = amounts.entry(line.child.clone()).or_insert_with(Q::zero);
                *next = next.add(&increment)?;
            }
        } else {
            requirements.insert(node.clone(), amount);
        }
    }
    Ok(Explosion {
        requirements,
        total_cost,
        lead_time,
        unit_rollups,
    })
}

/// Reduction over a node's own value and every descendant in its subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregation {
    /// Exact checked sum.
    Sum,
    /// Minimum value.
    Min,
    /// Maximum value.
    Max,
}

/// Roll up a single rooted tree, returning the aggregate at every node.
///
/// Values must cover exactly the nodes; signed quantities are allowed. Edges
/// point from parent to child. Parallel edges, multiple parents, disconnected
/// nodes and an empty hierarchy are not trees.
///
/// # Errors
/// Returns a concrete cycle, `NotTree`, invalid graph/value references, exact
/// arithmetic failures, or budget exhaustion.
pub fn hierarchical_rollup<N: Ord + Clone, Q: Quantity>(
    tree: &Graph<N>,
    root: &N,
    values: &BTreeMap<N, Q>,
    aggregation: Aggregation,
    budget: Budget,
) -> Result<BTreeMap<N, Q>, Error<N>> {
    let mut meter = Meter::new(budget, tree.nodes.len(), tree.edges.len())?;
    let graph = prepare(tree, &mut meter)?;
    graph.links(root)?;
    validate_values(&graph, values, false, &mut meter)?;
    let order = graph.order(&mut meter)?;
    for (node, links) in &graph.nodes {
        meter.step()?;
        let expected = usize::from(node != root);
        if links.incoming.len() != expected {
            return Err(Error::NotTree);
        }
    }
    let mut result: BTreeMap<N, Q> = BTreeMap::new();
    for node in order.iter().rev() {
        meter.step()?;
        let mut aggregate = value(values, node)?.clone();
        for child in &graph.links(node)?.out {
            meter.step()?;
            let child_value = result.get(child).ok_or(Error::Invariant)?;
            aggregate = match aggregation {
                Aggregation::Sum => aggregate.add(child_value)?,
                Aggregation::Min => aggregate.min(child_value.clone()),
                Aggregation::Max => aggregate.max(child_value.clone()),
            };
        }
        result.insert(node.clone(), aggregate);
    }
    Ok(result)
}
