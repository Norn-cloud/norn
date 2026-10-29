use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ArithmeticError;

/// Explicit resource ceilings for a single algorithm call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Maximum declared nodes, checked before allocating graph storage.
    pub max_nodes: usize,
    /// Maximum supplied edges (including parallel edges).
    pub max_edges: usize,
    /// Maximum metered work items across validation and all traversal passes.
    pub max_steps: usize,
}

/// Resource whose ceiling was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// Declared node count.
    Nodes,
    /// Declared edge count.
    Edges,
    /// Metered work items.
    Steps,
}

/// Deterministic resource refusal; no partial result is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// Exhausted resource.
    pub resource: Limit,
    /// Caller-supplied ceiling.
    pub limit: usize,
}

/// Typed refusal shared by all graph algorithms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error<N> {
    /// A resource ceiling was exceeded.
    BudgetExceeded(BudgetExceeded),
    /// A node was declared more than once.
    DuplicateNode(N),
    /// An edge, seed, demand or attribute references an undeclared node.
    UnknownNode(N),
    /// A required duration or value is absent.
    MissingValue(N),
    /// A duration, requirement, cost or lead time is negative.
    NegativeValue(N),
    /// One concrete directed cycle, with the first node repeated at the end.
    Cycle(Vec<N>),
    /// The supplied hierarchy is not a single rooted tree.
    NotTree,
    /// An exact arithmetic operation failed.
    Arithmetic(ArithmeticError),
    /// An internal graph invariant failed.
    Invariant,
}

impl<N: fmt::Debug> fmt::Display for Error<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "graph algorithm refused: {self:?}")
    }
}

impl<N: fmt::Debug> std::error::Error for Error<N> {}

impl<N> From<ArithmeticError> for Error<N> {
    fn from(value: ArithmeticError) -> Self {
        Self::Arithmetic(value)
    }
}

/// Directed graph. An edge `(a, b)` means `a` precedes or contains `b`.
///
/// Nodes must be declared exactly once; endpoints must be declared. Parallel
/// edges are accepted and counted individually. Ordering uses `Ord`, never hashing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Graph<N> {
    /// Declared nodes, including isolated nodes.
    pub nodes: Vec<N>,
    /// Directed edges in caller-supplied order.
    pub edges: Vec<(N, N)>,
}

/// Direction of an impact traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Follow outgoing edges.
    Downstream,
    /// Follow incoming edges.
    Upstream,
}

pub(crate) struct Meter {
    budget: Budget,
    steps: usize,
}

impl Meter {
    pub(crate) fn new<N>(budget: Budget, nodes: usize, edges: usize) -> Result<Self, Error<N>> {
        Self::check(nodes, budget.max_nodes, Limit::Nodes)?;
        Self::check(edges, budget.max_edges, Limit::Edges)?;
        Ok(Self { budget, steps: 0 })
    }

    fn check<N>(actual: usize, limit: usize, resource: Limit) -> Result<(), Error<N>> {
        if actual > limit {
            return Err(Error::BudgetExceeded(BudgetExceeded { resource, limit }));
        }
        Ok(())
    }

    pub(crate) fn step<N>(&mut self) -> Result<(), Error<N>> {
        if self.steps >= self.budget.max_steps {
            return Err(Error::BudgetExceeded(BudgetExceeded {
                resource: Limit::Steps,
                limit: self.budget.max_steps,
            }));
        }
        self.steps = self.steps.checked_add(1).ok_or(Error::Invariant)?;
        Ok(())
    }
}

pub(crate) struct Links<N> {
    pub(crate) out: Vec<N>,
    pub(crate) incoming: Vec<N>,
}

pub(crate) struct Prepared<N> {
    pub(crate) nodes: BTreeMap<N, Links<N>>,
}

impl<N: Ord + Clone> Prepared<N> {
    pub(crate) fn new<'a>(
        nodes: impl Iterator<Item = &'a N>,
        edges: impl Iterator<Item = (&'a N, &'a N)>,
        meter: &mut Meter,
    ) -> Result<Self, Error<N>>
    where
        N: 'a,
    {
        let mut graph = Self {
            nodes: BTreeMap::new(),
        };
        for node in nodes {
            meter.step()?;
            if graph
                .nodes
                .insert(
                    node.clone(),
                    Links {
                        out: Vec::new(),
                        incoming: Vec::new(),
                    },
                )
                .is_some()
            {
                return Err(Error::DuplicateNode(node.clone()));
            }
        }
        for (from, to) in edges {
            meter.step()?;
            graph
                .nodes
                .get_mut(from)
                .ok_or_else(|| Error::UnknownNode(from.clone()))?
                .out
                .push(to.clone());
            graph
                .nodes
                .get_mut(to)
                .ok_or_else(|| Error::UnknownNode(to.clone()))?
                .incoming
                .push(from.clone());
        }
        Ok(graph)
    }

    pub(crate) fn links(&self, node: &N) -> Result<&Links<N>, Error<N>> {
        self.nodes
            .get(node)
            .ok_or_else(|| Error::UnknownNode(node.clone()))
    }

    pub(crate) fn order(&self, meter: &mut Meter) -> Result<Vec<N>, Error<N>> {
        let mut degrees = BTreeMap::new();
        let mut ready = BTreeSet::new();
        for (node, links) in &self.nodes {
            meter.step()?;
            degrees.insert(node.clone(), links.incoming.len());
            if links.incoming.is_empty() {
                ready.insert(node.clone());
            }
        }
        let mut order = Vec::new();
        while let Some(node) = ready.pop_first() {
            meter.step()?;
            for next in &self.links(&node)?.out {
                meter.step()?;
                let degree = degrees.get_mut(next).ok_or(Error::Invariant)?;
                *degree = degree.checked_sub(1).ok_or(Error::Invariant)?;
                if *degree == 0 {
                    ready.insert(next.clone());
                }
            }
            order.push(node);
        }
        if order.len() != self.nodes.len() {
            return Err(Error::Cycle(self.cycle(meter)?));
        }
        Ok(order)
    }

    // Iterative DFS: active offsets reconstruct one back-edge cycle without recursion.
    fn cycle(&self, meter: &mut Meter) -> Result<Vec<N>, Error<N>> {
        let mut done = BTreeSet::new();
        let mut active = BTreeMap::new();
        let mut stack: Vec<(N, usize)> = Vec::new();
        for start in self.nodes.keys() {
            meter.step()?;
            if done.contains(start) {
                continue;
            }
            active.insert(start.clone(), 0);
            stack.push((start.clone(), 0));
            while let Some((node, offset)) = stack.last_mut() {
                meter.step()?;
                if let Some(next) = self.links(node)?.out.get(*offset) {
                    *offset = offset.checked_add(1).ok_or(Error::Invariant)?;
                    if let Some(first) = active.get(next) {
                        let mut cycle = Vec::new();
                        for (item, _) in stack.iter().skip(*first) {
                            meter.step()?;
                            cycle.push(item.clone());
                        }
                        meter.step()?;
                        cycle.push(next.clone());
                        return Ok(cycle);
                    }
                    if !done.contains(next) {
                        active.insert(next.clone(), stack.len());
                        stack.push((next.clone(), 0));
                    }
                } else {
                    let (finished, _) = stack.pop().ok_or(Error::Invariant)?;
                    active.remove(&finished);
                    done.insert(finished);
                }
            }
        }
        Err(Error::Invariant)
    }
}

pub(crate) fn prepare<N: Ord + Clone>(
    graph: &Graph<N>,
    meter: &mut Meter,
) -> Result<Prepared<N>, Error<N>> {
    Prepared::new(
        graph.nodes.iter(),
        graph.edges.iter().map(|(a, b)| (a, b)),
        meter,
    )
}

/// Return a topological order, choosing the smallest ready node first.
///
/// # Errors
/// Returns a concrete closed cycle, invalid input, or a budget refusal. Cycle
/// explanation shares the same meter as ordering and can itself exhaust it.
pub fn topological_order<N: Ord + Clone>(
    graph: &Graph<N>,
    budget: Budget,
) -> Result<Vec<N>, Error<N>> {
    let mut meter = Meter::new(budget, graph.nodes.len(), graph.edges.len())?;
    prepare(graph, &mut meter)?.order(&mut meter)
}

/// Return the union reachable from `seeds`, including every seed itself.
///
/// Cycles and parallel edges are allowed. Each node is visited once.
///
/// # Errors
/// Returns invalid graph/seed references or a budget refusal.
pub fn reachability<N: Ord + Clone>(
    graph: &Graph<N>,
    seeds: &[N],
    direction: Direction,
    budget: Budget,
) -> Result<BTreeSet<N>, Error<N>> {
    let mut meter = Meter::new(budget, graph.nodes.len(), graph.edges.len())?;
    let graph = prepare(graph, &mut meter)?;
    let mut found = BTreeSet::new();
    let mut pending = Vec::new();
    for seed in seeds {
        meter.step()?;
        graph.links(seed)?;
        if found.insert(seed.clone()) {
            pending.push(seed.clone());
        }
    }
    while let Some(node) = pending.pop() {
        meter.step()?;
        let links = graph.links(&node)?;
        let neighbors = match direction {
            Direction::Downstream => &links.out,
            Direction::Upstream => &links.incoming,
        };
        for next in neighbors {
            meter.step()?;
            if found.insert(next.clone()) {
                pending.push(next.clone());
            }
        }
    }
    Ok(found)
}
