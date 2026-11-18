//! "No dead ends" (LIFE-SOUND-1): option to complete over a modal transition graph.
//!
//! One core for both kinds of machine Norn checks: backend lifecycles
//! (`lifecycle_soundness`, `obligations/lifecycle/soundness`) and UI page statecharts
//! (UI-MACHINE-1). Each caller lowers its machine to a [`Net`], calls [`analyse`], and maps
//! the per-state [`StateVerdict`] to its own codes and pointers.
//!
//! Workflow-net soundness's option to complete is the CTL property `AG EF ending`, which no
//! LTL tool (TLC, Apalache, Quint) can state, so it is computed here as two fixpoints, written
//! in the notation of the `// rule:` comments so they can move to `norn-datalog` unchanged:
//!
//! ```text
//! reach(S)    :- init(S).
//! reach(T)    :- reach(S), edge(S, _, T).               // may or must
//! exit(S)     :- ending(S).
//! exit(S)     :- must_edge(S, _, T), exit(T).
//! may_exit(S) :- ending(S).
//! may_exit(S) :- edge(S, _, T), may_exit(T).
//! stuck(S)    :- reach(S), not exit(S).
//! unknown(S)  :- stuck(S), may_exit(S).                  // an exit exists only along may edges
//! trap(S)     :- stuck(S), not may_exit(S).
//! deadlock(S) :- not final(S), not leaves(S).            // leaves: an edge to another state
//! ```
//!
//! `reach` over-approximates (may edges count), so no stuck state is missed; `exit`
//! under-approximates (only must edges discharge), so a state is never passed on an edge that
//! might be disabled. A state with an exit only along may edges is [`Completion::Unknown`],
//! never a pass. Everything is linear in the size of the net.

use std::collections::{BTreeSet, VecDeque};

/// A UI machine composed with a backend lifecycle (#1236).
#[allow(
    dead_code,
    reason = "the UI-MACHINE-1 composition (#1236) calls it; L-ui lands the caller"
)]
pub(crate) mod compose;

/// What reaching a state means for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// A good final ending (a lifecycle's `completed` or unclassified terminal; a UI `terminal`).
    Good,
    /// A final ending after every dispatched effect was compensated.
    Compensated,
    /// A final ending before any uncompensated effect.
    Cancelled,
    /// A final ending with a visible breach and a remedy owed.
    BreachedVisible,
    /// A state a run may rest in indefinitely; not final, so it still needs an exit
    /// (a perpetual object's resting state; a UI `idle` state).
    Resting,
}

impl Ending {
    /// Final endings need no exit; a resting one does.
    pub(crate) const fn is_final(self) -> bool {
        !matches!(self, Self::Resting)
    }
}

/// Whether an edge is enabled for every concrete valuation the abstract state stands for
/// (`Must`) or only for some (`May`: a guard the analysis does not decide, an abstracted law).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modality {
    /// Enabled in every valuation; discharges option to complete.
    Must,
    /// Enabled in some valuation; counts for reachability only.
    May,
}

/// One edge of a [`Net`]. Built only through [`NetEdge::unguarded`], [`NetEdge::guarded`] or
/// [`NetEdge::evaluated`]: a caller says what it knows about the edge's guard and the core
/// decides the modality, so a guard that may never be enabled can never discharge option to
/// complete (coordinator ruling, 2026-11-13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetEdge {
    /// Source state index.
    pub(crate) from: usize,
    /// Target state index.
    pub(crate) to: usize,
    /// The caller's label for the edge (a transition index), echoed in witnesses.
    pub(crate) label: usize,
    /// Driven by an input (a person, an external event, a server outcome) rather than time alone.
    pub(crate) input: bool,
    modality: Modality,
}

impl NetEdge {
    /// An edge with no guard: enabled whenever its source state holds (must).
    pub(crate) const fn unguarded(from: usize, to: usize, label: usize, input: bool) -> Self {
        Self {
            from,
            to,
            label,
            input,
            modality: Modality::Must,
        }
    }

    /// An edge whose guard the caller has not decided (may): it counts for reachability, never
    /// for completion. This is the only constructor for a guarded edge over abstract states.
    pub(crate) const fn guarded(from: usize, to: usize, label: usize, input: bool) -> Self {
        Self {
            from,
            to,
            label,
            input,
            modality: Modality::May,
        }
    }

    /// An edge of a graph over exact valuations whose guards and laws were evaluated true on
    /// this source valuation (the PROOF-LIFECYCLE-3 rows): must, because the evaluation is the
    /// proof that the guard is enabled there.
    pub(crate) const fn evaluated(from: usize, to: usize, label: usize, input: bool) -> Self {
        Self {
            from,
            to,
            label,
            input,
            modality: Modality::Must,
        }
    }

    /// Must or may, as decided by the constructor.
    pub(crate) const fn modality(&self) -> Modality {
        self.modality
    }
}

/// A finite machine: states `0..states`, initial states, endings and edges.
#[derive(Debug, Clone, Default)]
pub(crate) struct Net {
    /// Number of states.
    pub(crate) states: usize,
    /// Initial state indices.
    pub(crate) initial: Vec<usize>,
    /// The ending of each state, if it is one (length `states`).
    pub(crate) endings: Vec<Option<Ending>>,
    /// Edges; a self-loop never counts as leaving its state.
    pub(crate) edges: Vec<NetEdge>,
}

/// Whether some ending is reachable from a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completion {
    /// Along must edges: proved.
    Must,
    /// Only along paths that use a may edge: Unknown, never a pass.
    Unknown,
    /// Along no path at all: refuted.
    None,
}

/// The verdict for one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StateVerdict {
    /// Reachable from an initial state along any edge.
    pub(crate) reachable: bool,
    /// Not a final ending and no edge leaves it for another state (absorbing).
    pub(crate) deadlock: bool,
    /// Whether an ending is reachable from it.
    pub(crate) completion: Completion,
}

impl StateVerdict {
    /// Reachable and refuted: a deadlock, or a trap (it has exits but no ending is reachable).
    pub(crate) const fn stuck(self) -> bool {
        self.reachable && (self.deadlock || matches!(self.completion, Completion::None))
    }
}

/// The analysis of a [`Net`].
#[derive(Debug, Clone)]
pub(crate) struct Verdict {
    /// One verdict per state, by index.
    pub(crate) states: Vec<StateVerdict>,
}

fn closure(seeds: impl IntoIterator<Item = usize>, next: &[Vec<usize>]) -> Vec<bool> {
    let mut seen = vec![false; next.len()];
    let mut queue = VecDeque::new();
    for seed in seeds {
        if let Some(slot) = seen.get_mut(seed)
            && !*slot
        {
            *slot = true;
            queue.push_back(seed);
        }
    }
    while let Some(state) = queue.pop_front() {
        for &successor in next.get(state).into_iter().flatten() {
            if let Some(slot) = seen.get_mut(successor)
                && !*slot
            {
                *slot = true;
                queue.push_back(successor);
            }
        }
    }
    seen
}

fn adjacency(net: &Net, keep: impl Fn(&NetEdge) -> bool, backward: bool) -> Vec<Vec<usize>> {
    let mut successors = vec![Vec::new(); net.states];
    for edge in net.edges.iter().filter(|edge| keep(edge)) {
        let (from, to) = if backward {
            (edge.to, edge.from)
        } else {
            (edge.from, edge.to)
        };
        if let Some(list) = successors.get_mut(from)
            && to < net.states
        {
            list.push(to);
        }
    }
    successors
}

/// Option to complete, deadlock and reachability for every state of `net`.
pub(crate) fn analyse(net: &Net) -> Verdict {
    let endings = || {
        net.endings
            .iter()
            .enumerate()
            .filter(|(_, ending)| ending.is_some())
            .map(|(state, _)| state)
    };
    let reach = closure(
        net.initial.iter().copied(),
        &adjacency(net, |_| true, false),
    );
    let exit = closure(
        endings(),
        &adjacency(net, |edge| edge.modality() == Modality::Must, true),
    );
    let may_exit = closure(endings(), &adjacency(net, |_| true, true));
    let leaves: BTreeSet<usize> = net
        .edges
        .iter()
        .filter(|edge| edge.from != edge.to && edge.to < net.states)
        .map(|edge| edge.from)
        .collect();
    let states = (0..net.states)
        .map(|state| {
            let ending = net.endings.get(state).copied().flatten();
            StateVerdict {
                reachable: reach.get(state).copied().unwrap_or(false),
                deadlock: !ending.is_some_and(Ending::is_final) && !leaves.contains(&state),
                completion: if exit.get(state).copied().unwrap_or(false) {
                    Completion::Must
                } else if may_exit.get(state).copied().unwrap_or(false) {
                    Completion::Unknown
                } else {
                    Completion::None
                },
            }
        })
        .collect();
    Verdict { states }
}

/// A shortest run from an initial state to `target`, as edge indices into `net.edges`
/// (declaration order breaks ties). `None` when `target` is unreachable.
pub(crate) fn witness(net: &Net, target: usize) -> Option<Vec<usize>> {
    let mut parent: Vec<Option<Option<usize>>> = vec![None; net.states];
    let mut queue = VecDeque::new();
    for &state in &net.initial {
        if let Some(slot) = parent.get_mut(state)
            && slot.is_none()
        {
            *slot = Some(None);
            queue.push_back(state);
        }
    }
    while let Some(state) = queue.pop_front() {
        if state == target {
            let mut run = Vec::new();
            let mut current = state;
            while let Some(Some(Some(edge))) = parent.get(current).copied() {
                run.push(edge);
                current = net.edges.get(edge)?.from;
            }
            run.reverse();
            return Some(run);
        }
        for (index, edge) in net.edges.iter().enumerate() {
            if edge.from == state
                && let Some(slot) = parent.get_mut(edge.to)
                && slot.is_none()
            {
                *slot = Some(Some(index));
                queue.push_back(edge.to);
            }
        }
    }
    None
}

/// States on a cycle of edges no input drives (a loop that runs on time alone). A self-loop
/// is a cycle.
pub(crate) fn input_free_cycles(net: &Net) -> Vec<bool> {
    let timed = adjacency(net, |edge| !edge.input, false);
    (0..net.states)
        .map(|state| {
            closure(timed.get(state).cloned().unwrap_or_default(), &timed)
                .get(state)
                .copied()
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "fixed test nets")]
mod tests {
    use super::*;

    fn edge(from: usize, to: usize, modality: Modality) -> NetEdge {
        match modality {
            Modality::Must => NetEdge::unguarded(from, to, to, true),
            Modality::May => NetEdge::guarded(from, to, to, true),
        }
    }

    /// 0 -> 1 -> 2 (good); 1 -> 3 <-> 4 (a trap); 0 -may-> 5 (good); 6 unreachable, absorbing.
    fn planted() -> Net {
        Net {
            states: 7,
            initial: vec![0],
            endings: vec![
                None,
                None,
                Some(Ending::Good),
                None,
                None,
                Some(Ending::Good),
                None,
            ],
            edges: vec![
                edge(0, 1, Modality::Must),
                edge(1, 2, Modality::Must),
                edge(1, 3, Modality::Must),
                edge(3, 4, Modality::Must),
                edge(4, 3, Modality::Must),
                edge(0, 5, Modality::May),
            ],
        }
    }

    #[test]
    fn a_trap_cycle_is_stuck_though_no_state_in_it_is_a_local_dead_end() {
        let verdict = analyse(&planted());
        for trap in [3, 4] {
            let state = verdict.states[trap];
            assert!(state.reachable && !state.deadlock, "{trap}");
            assert_eq!(state.completion, Completion::None, "{trap}");
            assert!(state.stuck(), "{trap}");
        }
        assert_eq!(verdict.states[1].completion, Completion::Must);
        assert!(!verdict.states[1].stuck());
        assert!(verdict.states[6].deadlock && !verdict.states[6].reachable);
        assert!(!verdict.states[6].stuck());
    }

    #[test]
    fn a_may_only_exit_is_unknown_and_never_a_pass() {
        let mut net = planted();
        net.edges.retain(|edge| (edge.from, edge.to) != (1, 2));
        let verdict = analyse(&net);
        assert_eq!(verdict.states[0].completion, Completion::Unknown);
        assert!(!verdict.states[0].stuck());
        assert_eq!(verdict.states[1].completion, Completion::None);
    }

    #[test]
    fn a_resting_state_still_needs_an_exit_and_a_self_loop_is_not_one() {
        let net = Net {
            states: 2,
            initial: vec![0],
            endings: vec![Some(Ending::Resting), Some(Ending::Good)],
            edges: vec![edge(0, 0, Modality::Must)],
        };
        let verdict = analyse(&net);
        assert!(verdict.states[0].deadlock);
        assert_eq!(verdict.states[0].completion, Completion::Must);
        assert!(!verdict.states[1].deadlock);
    }

    #[test]
    fn the_witness_is_a_shortest_run() {
        let net = planted();
        assert_eq!(witness(&net, 4), Some(vec![0, 2, 3]));
        assert_eq!(witness(&net, 0), Some(vec![]));
        assert_eq!(witness(&net, 6), None);
    }

    #[test]
    fn only_input_free_loops_are_flagged() {
        let mut net = planted();
        if let Some(edge) = net.edges.get_mut(3) {
            edge.input = false;
        }
        assert!(input_free_cycles(&net).iter().all(|flag| !flag));
        if let Some(edge) = net.edges.get_mut(4) {
            edge.input = false;
        }
        let flags = input_free_cycles(&net);
        assert_eq!(flags, vec![false, false, false, true, true, false, false]);
    }
}
