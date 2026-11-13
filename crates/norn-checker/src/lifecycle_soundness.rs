//! Lifecycle soundness, structural layer (LIFE-SOUND-1..8, Core 05; #1126).
//!
//! "No dead ends": every run of a lifecycle reaches a good or compensated ending. The local
//! check `lifecycle.validation.dead_end_state` only finds a state with no exit; a cycle with no
//! way out (a trap) passes it. This pass lowers the effective machine to a
//! `soundness::Net` and checks option to complete over the whole graph, with timers
//! and external events as edges so they are visible to it:
//!
//! - an unguarded transition, timer or event is a **must** edge; a guarded one is **may**, so
//!   a state whose only way to an ending crosses a guard is Unknown (`completion_unknown`),
//!   never a pass. Guards and laws are decided over finite rows by
//!   `obligations::lifecycle::soundness`, which also reports law-blocked exits;
//! - facts are never refused: a fact transition carries no guard (`fact_guarded`);
//! - every reachable non-terminal state handles every declared external event, or ignores it
//!   with a reason (`unhandled_event`); a state that only waits on the world has a deadline
//!   (`wait_without_deadline`); a deadline leads somewhere an ending is reachable
//!   (`deadline_without_remedy`); no loop runs on timers alone (`timer_cycle`);
//! - a dispatched effect is compensated before a `cancelled`, `compensated` or
//!   `breached_visible` ending (`compensation_not_closed`).
//!
//! What this establishes is structural and conditional on the declared edges; it adds no
//! runtime authority and changes no commit.

use std::collections::{BTreeSet, VecDeque};

use norn_ir::{Lifecycle, LifecycleOutcome, TransitionMode, TransitionTrigger};

use crate::lifecycles::{Edge, Machine};
use crate::soundness::{self, Completion, Ending, Modality, Net, NetEdge};

/// Stable `lifecycle.soundness.*` codes.
pub mod codes {
    /// An ending names an unknown state, repeats one, gives a non-terminal state a final
    /// outcome or a terminal state the `resting` outcome.
    pub const INVALID_ENDING: &str = "lifecycle.soundness.invalid_ending";
    /// An external event id is malformed or repeated, or an ignore entry is empty, unexplained,
    /// names an unknown state, or ignores the event where a transition handles it.
    pub const INVALID_EVENT: &str = "lifecycle.soundness.invalid_event";
    /// A trigger names an undeclared event or an invalid delay, a triggered transition is
    /// declared a request, or `compensatedBy` names no other transition.
    pub const INVALID_TRIGGER: &str = "lifecycle.soundness.invalid_trigger";
    /// A fact transition carries a guard, so it could be refused.
    pub const FACT_GUARDED: &str = "lifecycle.soundness.fact_guarded";
    /// A reachable state has exits but no ending is reachable from it.
    pub const TRAP: &str = "lifecycle.soundness.trap";
    /// An ending is reachable from a state only through a guarded transition.
    pub const COMPLETION_UNKNOWN: &str = "lifecycle.soundness.completion_unknown";
    /// The lifecycle declares no ending at all: no terminal and no resting state.
    pub const NO_ENDING: &str = "lifecycle.soundness.no_ending";
    /// A reachable non-terminal state neither handles nor ignores a declared event.
    pub const UNHANDLED_EVENT: &str = "lifecycle.soundness.unhandled_event";
    /// A state whose every exit waits on an external event has no timer.
    pub const WAIT_WITHOUT_DEADLINE: &str = "lifecycle.soundness.wait_without_deadline";
    /// A timer leads to a state from which no ending is reachable.
    pub const DEADLINE_WITHOUT_REMEDY: &str = "lifecycle.soundness.deadline_without_remedy";
    /// A cycle of transitions fires on timers alone.
    pub const TIMER_CYCLE: &str = "lifecycle.soundness.timer_cycle";
    /// A run can reach a cancelled, compensated or breached ending after dispatching an
    /// effect without its compensating transition.
    pub const COMPENSATION_NOT_CLOSED: &str = "lifecycle.soundness.compensation_not_closed";
    /// A reachable exit of a finite row is refused by governing laws (graph layer).
    pub const LAW_BLOCKED_EXIT: &str = "lifecycle.soundness.law_blocked_exit";
    /// A governing law refuses a fact transition (graph layer).
    pub const FACT_REFUSED: &str = "lifecycle.soundness.fact_refused";
    /// A defeasible `within` deadline could be cancelled by its exception in a state from
    /// which no declared good or compensated ending is reachable (LIFE-SOUND-9).
    pub const CANCELLATION_DEAD_END: &str = "lifecycle.soundness.cancellation_dead_end";
}

/// One finding: a code, a message and the member path under the lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) path: Vec<String>,
    /// Informational (an Unknown, never a pass), not an error.
    pub(crate) info: bool,
}

fn finding(code: &'static str, message: String, path: &[&str]) -> Finding {
    Finding {
        code,
        message,
        path: path.iter().map(|segment| (*segment).to_owned()).collect(),
        info: false,
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Shape of a declaration's soundness members, over its own enum `states`.
#[allow(
    clippy::too_many_lines,
    reason = "one pass per declaration member list"
)]
pub(crate) fn check_declaration(lifecycle: &Lifecycle, states: &[&str]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, ending) in lifecycle.endings.iter().enumerate() {
        let index = index.to_string();
        let path = ["endings", index.as_str(), "state"];
        let state = ending.state.as_str();
        let terminal = lifecycle.terminal.iter().any(|t| t == state);
        let problem = if !states.contains(&state) {
            Some(format!("ending {state:?} is not a state"))
        } else if !seen.insert(state) {
            Some(format!("ending {state:?} is listed twice"))
        } else if ending.outcome.is_final() && !terminal {
            Some(format!(
                "ending {state:?} has a final outcome but is not terminal; list it as terminal or make it resting"
            ))
        } else if !ending.outcome.is_final() && terminal {
            Some(format!(
                "terminal state {state:?} cannot be resting; give it a final outcome"
            ))
        } else {
            None
        };
        if let Some(message) = problem {
            out.push(finding(codes::INVALID_ENDING, message, &path));
        }
    }
    let mut ids = BTreeSet::new();
    for (index, event) in lifecycle.events.iter().enumerate() {
        let index = index.to_string();
        let id = event.id.as_str();
        if !valid_id(id) || !ids.insert(id) {
            out.push(finding(
                codes::INVALID_EVENT,
                format!("event id {id:?} is malformed or repeated"),
                &["events", index.as_str(), "id"],
            ));
        }
        for (position, ignore) in event.ignored_in.iter().enumerate() {
            let position = position.to_string();
            let path = ["events", index.as_str(), "ignoredIn", position.as_str()];
            if ignore.states.is_empty() || ignore.reason.trim().is_empty() {
                out.push(finding(
                    codes::INVALID_EVENT,
                    format!("event {id:?} is ignored without states or without a reason"),
                    &path,
                ));
            }
            for state in &ignore.states {
                if !states.contains(&state.as_str()) {
                    out.push(finding(
                        codes::INVALID_EVENT,
                        format!("event {id:?} is ignored in {state:?}, which is not a state"),
                        &path,
                    ));
                } else if lifecycle.transitions.iter().any(|transition| {
                    matches!(&transition.trigger, Some(TransitionTrigger::Event { event }) if event == id)
                        && transition.from.contains(state)
                }) {
                    out.push(finding(
                        codes::INVALID_EVENT,
                        format!("event {id:?} is both handled and ignored in {state:?}"),
                        &path,
                    ));
                }
            }
        }
    }
    for (index, transition) in lifecycle.transitions.iter().enumerate() {
        let index = index.to_string();
        let id = transition.id.as_str();
        let at = |member: &'static str| vec!["transitions", index.as_str(), member];
        match &transition.trigger {
            Some(TransitionTrigger::Timer { millis })
                if crate::deontic::canonical_positive_millis(millis).is_none() =>
            {
                out.push(finding(
                    codes::INVALID_TRIGGER,
                    format!("transition {id:?} has timer delay {millis:?}; use positive canonical milliseconds"),
                    &at("trigger"),
                ));
            }
            Some(TransitionTrigger::Event { event })
                if !lifecycle
                    .events
                    .iter()
                    .any(|declared| &declared.id == event) =>
            {
                out.push(finding(
                    codes::INVALID_TRIGGER,
                    format!("transition {id:?} is triggered by undeclared event {event:?}"),
                    &at("trigger"),
                ));
            }
            Some(TransitionTrigger::Timer { .. } | TransitionTrigger::Event { .. }) | None => {}
        }
        if transition.trigger.is_some() && transition.mode == Some(TransitionMode::Request) {
            out.push(finding(
                codes::INVALID_TRIGGER,
                format!(
                    "transition {id:?} is fired by a timer or event, so it is a fact, not a request"
                ),
                &at("mode"),
            ));
        }
        if let Some(compensation) = &transition.compensated_by
            && (compensation == &transition.id
                || !lifecycle.transitions.iter().any(|t| &t.id == compensation))
        {
            out.push(finding(
                codes::INVALID_TRIGGER,
                format!(
                    "transition {id:?} is compensated by {:?}, which is not another transition of the lifecycle",
                    compensation.as_str()
                ),
                &at("compensatedBy"),
            ));
        }
        if transition.is_fact() && !transition.guards.is_empty() {
            out.push(finding(
                codes::FACT_GUARDED,
                format!(
                    "fact transition {id:?} has a guard, so it could be refused; facts commit, post a variance or shortfall, or quarantine"
                ),
                &at("guards"),
            ));
        }
    }
    out
}

/// The ending a state of `machine` is, if any. A terminal state without a declared outcome is
/// an unclassified (good) ending.
fn ending(machine: &Machine<'_>, state: &str) -> Option<(Ending, Option<LifecycleOutcome>)> {
    let declared = machine
        .endings
        .iter()
        .find(|(name, _)| *name == state)
        .map(|(_, outcome)| *outcome);
    match declared {
        Some(LifecycleOutcome::Completed) => Some((Ending::Good, declared)),
        Some(LifecycleOutcome::Compensated) => Some((Ending::Compensated, declared)),
        Some(LifecycleOutcome::Cancelled) => Some((Ending::Cancelled, declared)),
        Some(LifecycleOutcome::BreachedVisible) => Some((Ending::BreachedVisible, declared)),
        Some(LifecycleOutcome::Resting) => Some((Ending::Resting, declared)),
        None => machine
            .terminal
            .contains(state)
            .then_some((Ending::Good, None)),
    }
}

fn index(machine: &Machine<'_>, state: &str) -> Option<usize> {
    machine.states.iter().position(|name| *name == state)
}

fn is_timer(edge: &Edge<'_>) -> bool {
    matches!(edge.trigger, Some(TransitionTrigger::Timer { .. }))
}

/// The net of `machine`: one state per enum value, one edge per (source, transition).
pub(crate) fn net(machine: &Machine<'_>) -> Net {
    let mut net = Net {
        states: machine.states.len(),
        initial: machine
            .states
            .iter()
            .enumerate()
            .filter(|(_, state)| machine.initial.contains(*state))
            .map(|(position, _)| position)
            .collect(),
        endings: machine
            .states
            .iter()
            .map(|state| ending(machine, state).map(|(ending, _)| ending))
            .collect(),
        edges: Vec::new(),
    };
    for (label, edge) in machine.transitions.iter().enumerate() {
        let Some(to) = index(machine, edge.to) else {
            continue;
        };
        for from in machine
            .states
            .iter()
            .filter(|state| edge.from.contains(*state))
        {
            if let Some(from) = index(machine, from) {
                let input = !is_timer(edge);
                net.edges.push(if edge.guards.is_empty() {
                    NetEdge::unguarded(from, to, label, input)
                } else {
                    NetEdge::guarded(from, to, label, input)
                });
            }
        }
    }
    net
}

fn run(machine: &Machine<'_>, net: &Net, edges: &[usize]) -> String {
    let steps: Vec<String> = edges
        .iter()
        .filter_map(|edge| net.edges.get(*edge))
        .filter_map(|edge| {
            let transition = machine.transitions.get(edge.label)?;
            let to = machine.states.get(edge.to)?;
            Some(format!("{} -> {to}", transition.id.as_str()))
        })
        .collect();
    let start = edges
        .first()
        .and_then(|edge| net.edges.get(*edge))
        .and_then(|edge| machine.states.get(edge.from))
        .copied()
        .unwrap_or_default();
    if steps.is_empty() {
        String::new()
    } else {
        format!("; shortest run: {start}, {}", steps.join(", "))
    }
}

/// Option to complete, events, deadlines and compensation over the effective `machine`.
#[allow(
    clippy::too_many_lines,
    reason = "one pass per LIFE-SOUND property over the same net"
)]
pub(crate) fn check_machine(machine: &Machine<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    if machine.initial.is_empty() {
        return out;
    }
    let net = net(machine);
    // D84: a chart that declares no LIFE-SOUND member keeps its admission output except for
    // refuted findings; Unknown and a missing ending are reported once the chart opts in.
    let opted_in = !machine.endings.is_empty()
        || !machine.events.is_empty()
        || machine.transitions.iter().any(|edge| {
            edge.trigger.is_some() || edge.mode.is_some() || edge.compensated_by.is_some()
        });
    if net.endings.iter().all(Option::is_none) {
        if !opted_in {
            return out;
        }
        out.push(Finding {
            info: true,
            ..finding(
                codes::NO_ENDING,
                "the lifecycle declares no terminal or resting state, so no run can complete; declare its endings (a perpetual object's resting states)".into(),
                &[],
            )
        });
        return out;
    }
    let verdict = soundness::analyse(&net);
    let state_name = |state: usize| machine.states.get(state).copied().unwrap_or_default();
    let mut stuck = BTreeSet::new();
    for (state, outcome) in verdict.states.iter().enumerate() {
        // A state without any exit is `lifecycle.validation.dead_end_state`'s.
        if !outcome.reachable || outcome.deadlock {
            continue;
        }
        let name = state_name(state);
        let trace = soundness::witness(&net, state)
            .map(|edges| run(machine, &net, &edges))
            .unwrap_or_default();
        match outcome.completion {
            Completion::Must => {}
            Completion::None => {
                stuck.insert(state);
                out.push(finding(
                    codes::TRAP,
                    format!(
                        "no ending is reachable from state {name:?}: every run through it cycles without completing; add an exit, a remedy, or declare an ending{trace}"
                    ),
                    &[],
                ));
            }
            Completion::Unknown if !opted_in => {}
            Completion::Unknown => out.push(Finding {
                info: true,
                ..finding(
                    codes::COMPLETION_UNKNOWN,
                    format!(
                        "an ending is reachable from state {name:?} only through a guarded transition; the finite lifecycle graph decides it{trace}"
                    ),
                    &[],
                )
            }),
        }
    }
    let reachable = |state: &str| {
        index(machine, state)
            .and_then(|position| verdict.states.get(position))
            .is_some_and(|outcome| outcome.reachable)
    };
    // LIFE-SOUND-6: every reachable non-terminal state handles or ignores every event.
    for event in &machine.events {
        for state in machine.states.iter().copied() {
            if machine.terminal.contains(state) || !reachable(state) {
                continue;
            }
            let handled = machine.transitions.iter().any(|edge| {
                edge.from.contains(state)
                    && matches!(edge.trigger, Some(TransitionTrigger::Event { event: id }) if id == event.id)
            });
            let ignored = event
                .ignored_in
                .iter()
                .any(|(states, _)| states.contains(state));
            if !handled && !ignored {
                out.push(finding(
                    codes::UNHANDLED_EVENT,
                    format!(
                        "state {state:?} neither handles nor ignores event {:?}; add a transition it triggers, or ignore it there with a reason",
                        event.id
                    ),
                    &[],
                ));
            }
        }
    }
    // LIFE-SOUND-7: waiting on the world needs a deadline, and a deadline needs a remedy.
    for state in machine.states.iter().copied() {
        if machine.terminal.contains(state) || !reachable(state) {
            continue;
        }
        let exits: Vec<&Edge<'_>> = machine
            .transitions
            .iter()
            .filter(|edge| edge.from.contains(state))
            .collect();
        if !exits.is_empty()
            && exits
                .iter()
                .all(|edge| matches!(edge.trigger, Some(TransitionTrigger::Event { .. })))
        {
            out.push(finding(
                codes::WAIT_WITHOUT_DEADLINE,
                format!(
                    "state {state:?} only waits on external events; add a timer transition so the wait has a deadline"
                ),
                &[],
            ));
        }
    }
    for edge in machine.transitions.iter().filter(|edge| is_timer(edge)) {
        if let Some(to) = index(machine, edge.to)
            && edge.from.iter().any(|from| reachable(from))
            && verdict
                .states
                .get(to)
                .is_some_and(|outcome| outcome.deadlock || outcome.completion == Completion::None)
        {
            out.push(finding(
                codes::DEADLINE_WITHOUT_REMEDY,
                format!(
                    "timer transition {:?} leads to {:?}, from which no ending is reachable; route the deadline to a remedy or an ending",
                    edge.id.as_str(),
                    edge.to
                ),
                &[],
            ));
        }
    }
    let cycles = soundness::input_free_cycles(&net);
    for (state, on_cycle) in cycles.iter().enumerate() {
        if *on_cycle {
            out.push(finding(
                codes::TIMER_CYCLE,
                format!(
                    "state {:?} is on a cycle of timer transitions; a loop needs an input or an exit",
                    state_name(state)
                ),
                &[],
            ));
        }
    }
    check_compensation(machine, &mut out);
    out
}

/// LIFE-SOUND-8 over the product (state, effect owed) for each dispatching transition.
fn check_compensation(machine: &Machine<'_>, out: &mut Vec<Finding>) {
    for (dispatch, edge) in machine.transitions.iter().enumerate() {
        let Some(compensation) = edge.compensated_by else {
            continue;
        };
        let Some(compensate) = machine
            .transitions
            .iter()
            .position(|other| other.id == compensation)
        else {
            continue;
        };
        let states = machine.states.len();
        let improper = |state: &str| {
            matches!(
                ending(machine, state),
                Some((
                    Ending::Cancelled | Ending::Compensated | Ending::BreachedVisible,
                    _
                ))
            )
        };
        // Product state `2 * state + owed`.
        let mut product = Net {
            states: states.saturating_mul(2),
            initial: machine
                .states
                .iter()
                .enumerate()
                .filter(|(_, state)| machine.initial.contains(*state))
                .map(|(position, _)| position.saturating_mul(2))
                .collect(),
            endings: vec![None; states.saturating_mul(2)],
            edges: Vec::new(),
        };
        for (label, transition) in machine.transitions.iter().enumerate() {
            let Some(to) = index(machine, transition.to) else {
                continue;
            };
            for from in machine
                .states
                .iter()
                .filter(|state| transition.from.contains(*state))
            {
                let Some(from) = index(machine, from) else {
                    continue;
                };
                for owed in 0..2_usize {
                    let after = if label == dispatch {
                        1
                    } else if label == compensate {
                        0
                    } else {
                        owed
                    };
                    // Reachability only: every declared step counts.
                    product.edges.push(NetEdge::guarded(
                        from.saturating_mul(2).saturating_add(owed),
                        to.saturating_mul(2).saturating_add(after),
                        label,
                        true,
                    ));
                }
            }
        }
        let verdict = soundness::analyse(&product);
        for (position, state) in machine.states.iter().copied().enumerate() {
            let owed = position.saturating_mul(2).saturating_add(1);
            if improper(state)
                && verdict
                    .states
                    .get(owed)
                    .is_some_and(|outcome| outcome.reachable)
            {
                let trace = soundness::witness(&product, owed)
                    .map(|edges| {
                        edges
                            .iter()
                            .filter_map(|edge| product.edges.get(*edge))
                            .filter_map(|edge| machine.transitions.get(edge.label))
                            .map(|transition| transition.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                out.push(finding(
                    codes::COMPENSATION_NOT_CLOSED,
                    format!(
                        "a run can end in {state:?} after {:?} dispatched its effect without {:?} compensating it; route the path through the compensation (run: {trace})",
                        edge.id.as_str(),
                        compensation.as_str()
                    ),
                    &[],
                ));
            }
        }
    }
}

/// Why a defeasible `within` deadline is refused (LIFE-SOUND-9; ruling on #1313).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationRefusal {
    /// The trigger is not a transition of the lifecycle.
    UnknownTrigger,
    /// Some cancelling state cannot be shown to reach a declared good or compensated ending.
    DeadEnd(CancellationDeadEnd),
}

/// The cancelling states that fail, and the smallest blocking set found for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationDeadEnd {
    /// Every failing cancelling state, in presentation order.
    pub states: Vec<String>,
    /// Failing states from which no good or compensated ending is reachable at all.
    pub unreachable: Vec<String>,
    /// For the other failing states: the guarded transitions on a path to a good ending
    /// that crosses the fewest guards, per state, unioned. The proof holds once these
    /// guards are shown to hold there (or are removed).
    pub blocking: Vec<String>,
}

impl CancellationDeadEnd {
    /// One-line explanation for the refusal diagnostic.
    #[must_use]
    pub fn message(&self) -> String {
        let mut parts = vec![format!(
            "an exception can cancel the deadline in {:?}, from which no declared good or compensated ending is proved reachable",
            self.states
        )];
        if !self.unreachable.is_empty() {
            parts.push(format!(
                "{:?} reach no good or compensated ending at all",
                self.unreachable
            ));
        }
        if !self.blocking.is_empty() {
            parts.push(format!(
                "smallest blocking set: guarded {:?}",
                self.blocking
            ));
        }
        parts.join("; ")
    }
}

fn good_outcome(outcome: Option<LifecycleOutcome>) -> bool {
    matches!(
        outcome,
        Some(
            LifecycleOutcome::Completed | LifecycleOutcome::Compensated | LifecycleOutcome::Resting
        )
    )
}

/// LIFE-SOUND-9: a defeasible `within` deadline triggered by `trigger` may be admitted only if,
/// in every live state where its exception could cancel the open activation (every non-terminal
/// state reachable from the trigger's target, guards treated as satisfiable), a declared good or compensated
/// ending (`completed`, `compensated`, `resting`) is reachable along unguarded edges. An
/// unclassified terminal is not a declared ending. The relaxation is gated by this proof, never
/// a default.
///
/// # Errors
/// [`CancellationRefusal`]: an unknown trigger, or the failing states with the smallest
/// blocking set.
pub fn cancellation_gate(
    lifecycle: &Lifecycle,
    states: &[String],
    trigger: &str,
) -> Result<(), CancellationRefusal> {
    let names: Vec<&str> = states.iter().map(String::as_str).collect();
    let machine = Machine::declared(lifecycle, names);
    let opening = machine
        .transitions
        .iter()
        .find(|edge| edge.id.as_str() == trigger)
        .ok_or(CancellationRefusal::UnknownTrigger)?;
    let start = index(&machine, opening.to).ok_or(CancellationRefusal::UnknownTrigger)?;
    let mut graph = net(&machine);
    graph.endings = machine
        .states
        .iter()
        .map(|state| {
            ending(&machine, state)
                .filter(|(_, outcome)| good_outcome(*outcome))
                .map(|(ending, _)| ending)
        })
        .collect();
    let verdict = soundness::analyse(&graph);
    let mut open = vec![false; graph.states];
    let mut queue = VecDeque::from([start]);
    if let Some(slot) = open.get_mut(start) {
        *slot = true;
    }
    while let Some(state) = queue.pop_front() {
        for edge in graph.edges.iter().filter(|edge| edge.from == state) {
            if let Some(slot) = open.get_mut(edge.to)
                && !*slot
            {
                *slot = true;
                queue.push_back(edge.to);
            }
        }
    }
    let mut failure = CancellationDeadEnd {
        states: Vec::new(),
        unreachable: Vec::new(),
        blocking: Vec::new(),
    };
    let mut blocking = BTreeSet::new();
    for (state, outcome) in verdict.states.iter().enumerate() {
        let name = machine.states.get(state).copied().unwrap_or_default();
        // A terminal state has already ended; cancelling there opens no new dead end.
        if !open.get(state).copied().unwrap_or(false)
            || outcome.completion == Completion::Must
            || machine.terminal.contains(name)
        {
            continue;
        }
        failure.states.push(name.to_owned());
        match fewest_guards(&graph, state) {
            Some(labels) => {
                blocking.extend(
                    labels
                        .into_iter()
                        .filter_map(|label| machine.transitions.get(label))
                        .map(|edge| edge.id.as_str().to_owned()),
                );
            }
            None => failure.unreachable.push(name.to_owned()),
        }
    }
    if failure.states.is_empty() {
        return Ok(());
    }
    failure.blocking = blocking.into_iter().collect();
    Err(CancellationRefusal::DeadEnd(failure))
}

/// The labels of the may edges on a path from `from` to an ending crossing the fewest may
/// edges (0-1 BFS), or `None` when no ending is reachable.
fn fewest_guards(net: &Net, from: usize) -> Option<Vec<usize>> {
    let mut cost = vec![usize::MAX; net.states];
    let mut parent: Vec<Option<usize>> = vec![None; net.states];
    let mut queue = VecDeque::from([from]);
    *cost.get_mut(from)? = 0;
    while let Some(state) = queue.pop_front() {
        let here = cost.get(state).copied()?;
        if net.endings.get(state).copied().flatten().is_some() {
            let mut labels = Vec::new();
            let mut current = state;
            while let Some(edge) = parent.get(current).copied().flatten() {
                let edge = net.edges.get(edge)?;
                if edge.modality() == Modality::May {
                    labels.push(edge.label);
                }
                current = edge.from;
            }
            labels.reverse();
            return Some(labels);
        }
        for (position, edge) in net.edges.iter().enumerate() {
            if edge.from != state {
                continue;
            }
            let step = usize::from(edge.modality() == Modality::May);
            let total = here.saturating_add(step);
            if let Some(known) = cost.get_mut(edge.to)
                && total < *known
            {
                *known = total;
                if let Some(slot) = parent.get_mut(edge.to) {
                    *slot = Some(position);
                }
                if step == 0 {
                    queue.push_front(edge.to);
                } else {
                    queue.push_back(edge.to);
                }
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "planted dead-end fixtures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lifecycle(value: serde_json::Value) -> Lifecycle {
        serde_json::from_value(value).expect("lifecycle")
    }

    fn codes_of(lifecycle: &Lifecycle, states: &[&str]) -> Vec<&'static str> {
        let declared = check_declaration(lifecycle, states);
        let machine = Machine::declared(lifecycle, states.to_vec());
        declared
            .into_iter()
            .chain(check_machine(&machine))
            .map(|finding| finding.code)
            .collect()
    }

    const ORDER: [&str; 5] = ["draft", "confirmed", "shipped", "cancelled", "review"];

    fn order(extra: &serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "property": "status",
            "initial": ["draft"],
            "terminal": ["shipped", "cancelled"],
            "endings": [
                {"state": "shipped", "outcome": "completed"},
                {"state": "cancelled", "outcome": "cancelled"}
            ],
            "transitions": [
                {"id": "confirm", "from": ["draft"], "to": "confirmed"},
                {"id": "discard", "from": ["draft"], "to": "cancelled"},
                {"id": "ship", "from": ["confirmed"], "to": "shipped", "mode": "fact"},
                {"id": "cancel", "from": ["confirmed"], "to": "cancelled"},
                {"id": "escalate", "from": ["confirmed"], "to": "review"},
                {"id": "resolve", "from": ["review"], "to": "confirmed"}
            ]
        });
        for (key, value) in extra.as_object().expect("object") {
            base[key] = value.clone();
        }
        base
    }

    #[test]
    fn a_sound_chart_has_no_findings() {
        assert_eq!(
            codes_of(&lifecycle(order(&json!({}))), &ORDER),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_planted_trap_is_found_though_every_state_has_an_exit() {
        let mut chart = order(&json!({}));
        // review <-> appeal: both have exits, neither reaches an ending.
        chart["transitions"] = json!([
            {"id": "confirm", "from": ["draft"], "to": "confirmed"},
            {"id": "discard", "from": ["draft"], "to": "cancelled"},
            {"id": "ship", "from": ["confirmed"], "to": "shipped"},
            {"id": "escalate", "from": ["confirmed"], "to": "review"},
            {"id": "appeal", "from": ["review"], "to": "draft_review"},
            {"id": "reopen", "from": ["draft_review"], "to": "review"}
        ]);
        let states = [
            "draft",
            "confirmed",
            "shipped",
            "cancelled",
            "review",
            "draft_review",
        ];
        let chart = lifecycle(chart);
        let machine = Machine::declared(&chart, states.to_vec());
        assert!(machine.dead_ends().is_empty(), "the local check passes it");
        let findings = check_machine(&machine);
        let traps: Vec<_> = findings.iter().filter(|f| f.code == codes::TRAP).collect();
        assert_eq!(traps.len(), 2, "{findings:?}");
        assert!(
            traps[0]
                .message
                .contains("shortest run: draft, confirm -> confirmed, escalate -> review"),
            "{}",
            traps[0].message
        );
    }

    #[test]
    fn a_guarded_only_route_is_unknown_not_a_pass() {
        let mut chart = order(&json!({}));
        chart["transitions"][1]["guards"] = json!(["self.total == 0"]);
        chart["transitions"][0]["guards"] = json!(["self.total > 0"]);
        let findings = check_machine(&Machine::declared(
            &lifecycle(chart.clone()),
            ORDER.to_vec(),
        ));
        assert_eq!(
            findings
                .iter()
                .map(|f| (f.code, f.info))
                .collect::<Vec<_>>(),
            vec![(codes::COMPLETION_UNKNOWN, true)]
        );
        // Without any LIFE-SOUND member the Unknown is left to the bounded graph.
        let mut legacy = chart;
        legacy["endings"] = json!([]);
        if let Some(ship) = legacy["transitions"][2].as_object_mut() {
            ship.remove("mode");
        }
        assert!(check_machine(&Machine::declared(&lifecycle(legacy), ORDER.to_vec())).is_empty());
    }

    #[test]
    fn a_guarded_fact_could_be_refused() {
        let mut chart = order(&json!({}));
        chart["transitions"][2]["guards"] = json!(["self.paid"]);
        assert_eq!(
            codes_of(&lifecycle(chart), &ORDER),
            vec![codes::FACT_GUARDED]
        );
    }

    #[test]
    fn events_are_handled_or_ignored_in_every_reachable_live_state() {
        let chart = order(&json!({
            "events": [
                {"id": "carrier_lost"},
                {"id": "carrier_ping", "ignoredIn": [{"states": ["draft", "confirmed", "review"], "reason": "telemetry only"}]}
            ]
        }));
        let mut handled = chart.clone();
        handled["transitions"]
            .as_array_mut()
            .expect("transitions")
            .push(json!({"id": "lost", "from": ["confirmed", "review"], "to": "cancelled", "trigger": {"kind": "event", "event": "carrier_lost"}}));
        assert_eq!(
            codes_of(&lifecycle(chart), &ORDER),
            vec![codes::UNHANDLED_EVENT; 3],
            "draft, confirmed and review miss carrier_lost"
        );
        let mut both = handled.clone();
        both["events"][0]["ignoredIn"] =
            json!([{"states": ["draft"], "reason": "not yet shipped"}]);
        assert_eq!(codes_of(&lifecycle(both), &ORDER), Vec::<&str>::new());
        assert_eq!(
            codes_of(&lifecycle(handled), &ORDER),
            vec![codes::UNHANDLED_EVENT]
        );
    }

    #[test]
    fn a_wait_needs_a_deadline_and_the_deadline_a_remedy() {
        let mut chart = order(&json!({"events": [{"id": "approved"}]}));
        chart["terminal"] = json!(["shipped", "cancelled"]);
        chart["transitions"] = json!([
            {"id": "confirm", "from": ["draft"], "to": "confirmed"},
            {"id": "discard", "from": ["draft"], "to": "cancelled"},
            {"id": "ship", "from": ["confirmed"], "to": "shipped", "trigger": {"kind": "event", "event": "approved"}},
            {"id": "escalate", "from": ["review"], "to": "confirmed", "trigger": {"kind": "event", "event": "approved"}}
        ]);
        let waits = codes_of(
            &lifecycle(chart.clone()),
            &["draft", "confirmed", "shipped", "cancelled"],
        );
        assert_eq!(
            waits,
            vec![codes::UNHANDLED_EVENT, codes::WAIT_WITHOUT_DEADLINE]
        );
        let transitions = chart["transitions"].as_array_mut().expect("transitions");
        transitions.push(json!({"id": "expire", "from": ["confirmed"], "to": "review", "trigger": {"kind": "timer", "millis": "86400000"}}));
        transitions.push(json!({"id": "retry", "from": ["review"], "to": "review_again", "trigger": {"kind": "timer", "millis": "60000"}}));
        transitions.push(json!({"id": "again", "from": ["review_again"], "to": "review", "trigger": {"kind": "timer", "millis": "60000"}}));
        chart["events"][0]["ignoredIn"] =
            json!([{"states": ["draft", "review_again"], "reason": "nothing to approve"}]);
        let states = [
            "draft",
            "confirmed",
            "shipped",
            "cancelled",
            "review",
            "review_again",
        ];
        let found = codes_of(&lifecycle(chart), &states);
        assert!(!found.contains(&codes::WAIT_WITHOUT_DEADLINE), "{found:?}");
        assert!(found.contains(&codes::TIMER_CYCLE), "{found:?}");
        // review escalates back to confirmed, so the deadline has a remedy.
        assert!(
            !found.contains(&codes::DEADLINE_WITHOUT_REMEDY),
            "{found:?}"
        );
    }

    #[test]
    fn a_deadline_into_a_trap_has_no_remedy() {
        let mut chart = order(&json!({}));
        chart["transitions"] = json!([
            {"id": "confirm", "from": ["draft"], "to": "confirmed"},
            {"id": "ship", "from": ["confirmed"], "to": "shipped"},
            {"id": "expire", "from": ["confirmed"], "to": "review", "trigger": {"kind": "timer", "millis": "1000"}},
            {"id": "poke", "from": ["review"], "to": "cancelled_review"},
            {"id": "unpoke", "from": ["cancelled_review"], "to": "review"}
        ]);
        let states = [
            "draft",
            "confirmed",
            "shipped",
            "cancelled",
            "review",
            "cancelled_review",
        ];
        let found = codes_of(&lifecycle(chart), &states);
        assert!(found.contains(&codes::DEADLINE_WITHOUT_REMEDY), "{found:?}");
        assert!(found.contains(&codes::TRAP), "{found:?}");
    }

    #[test]
    fn a_cancel_after_dispatch_must_run_the_compensation() {
        let mut chart = order(&json!({}));
        chart["transitions"][0]["compensatedBy"] = json!("refund");
        let found = codes_of(&lifecycle(chart.clone()), &ORDER);
        assert_eq!(
            found,
            vec![codes::INVALID_TRIGGER],
            "refund is not declared"
        );
        chart["transitions"] = json!([
            {"id": "confirm", "from": ["draft"], "to": "confirmed", "compensatedBy": "refund"},
            {"id": "discard", "from": ["draft"], "to": "cancelled"},
            {"id": "ship", "from": ["confirmed"], "to": "shipped"},
            {"id": "cancel", "from": ["confirmed"], "to": "cancelled"},
            {"id": "escalate", "from": ["confirmed"], "to": "review"},
            {"id": "refund", "from": ["review"], "to": "cancelled"}
        ]);
        let found = codes_of(&lifecycle(chart.clone()), &ORDER);
        assert_eq!(
            found,
            vec![codes::COMPENSATION_NOT_CLOSED],
            "cancel skips the refund"
        );
        chart["transitions"][3]["to"] = json!("review");
        chart["transitions"][3]["from"] = json!(["confirmed"]);
        chart["transitions"].as_array_mut().expect("t").remove(4);
        assert_eq!(codes_of(&lifecycle(chart), &ORDER), Vec::<&str>::new());
    }

    #[test]
    fn a_cancellable_deadline_is_admitted_only_where_every_cancelling_state_still_ends_well() {
        // confirm opens the deadline; shipped is completed, refunded compensated.
        let chart = || {
            json!({
                "property": "status", "initial": ["draft"],
                "terminal": ["shipped", "cancelled", "refunded"],
                "endings": [
                    {"state": "shipped", "outcome": "completed"},
                    {"state": "cancelled", "outcome": "cancelled"},
                    {"state": "refunded", "outcome": "compensated"}
                ],
                "transitions": [
                    {"id": "confirm", "from": ["draft"], "to": "confirmed"},
                    {"id": "ship", "from": ["confirmed"], "to": "shipped"},
                    {"id": "escalate", "from": ["confirmed"], "to": "review"},
                    {"id": "refund", "from": ["review"], "to": "refunded"}
                ]
            })
        };
        let states: Vec<String> = [
            "draft",
            "confirmed",
            "review",
            "shipped",
            "cancelled",
            "refunded",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            cancellation_gate(&lifecycle(chart()), &states, "confirm"),
            Ok(())
        );
        assert_eq!(
            cancellation_gate(&lifecycle(chart()), &states, "post"),
            Err(CancellationRefusal::UnknownTrigger)
        );
        // review can now only be cancelled: a cancelled ending is not good or compensated.
        let mut dead = chart();
        dead["transitions"][3] = json!({"id": "drop", "from": ["review"], "to": "cancelled"});
        let Err(CancellationRefusal::DeadEnd(failure)) =
            cancellation_gate(&lifecycle(dead), &states, "confirm")
        else {
            panic!("review reaches only a cancelled ending");
        };
        assert_eq!(failure.states, ["review"]);
        assert_eq!(failure.unreachable, ["review"]);
        assert!(failure.blocking.is_empty());
        // a guarded refund leaves review Unknown: the guard is the smallest blocking set.
        let mut guarded = chart();
        guarded["transitions"][3]["guards"] = json!(["self.paid"]);
        let Err(CancellationRefusal::DeadEnd(failure)) =
            cancellation_gate(&lifecycle(guarded), &states, "confirm")
        else {
            panic!("review ends well only through a guard");
        };
        assert_eq!(failure.states, ["review"]);
        assert!(failure.unreachable.is_empty());
        assert_eq!(failure.blocking, ["refund"]);
        assert!(failure.message().contains("smallest blocking set"));
        // An unclassified terminal is not a declared good ending.
        let mut undeclared = chart();
        undeclared["endings"] = json!([]);
        assert!(matches!(
            cancellation_gate(&lifecycle(undeclared), &states, "confirm"),
            Err(CancellationRefusal::DeadEnd(_))
        ));
    }

    #[test]
    fn declaration_shape_is_checked() {
        let chart = order(&json!({
            "endings": [
                {"state": "shipped", "outcome": "completed"},
                {"state": "shipped", "outcome": "completed"},
                {"state": "review", "outcome": "cancelled"},
                {"state": "cancelled", "outcome": "resting"},
                {"state": "nowhere", "outcome": "completed"}
            ],
            "events": [{"id": "Bad"}, {"id": "ok", "ignoredIn": [{"states": [], "reason": " "}]}]
        }));
        let found: Vec<_> = check_declaration(&lifecycle(chart), &ORDER)
            .into_iter()
            .map(|finding| finding.code)
            .collect();
        assert_eq!(
            found,
            vec![
                codes::INVALID_ENDING,
                codes::INVALID_ENDING,
                codes::INVALID_ENDING,
                codes::INVALID_ENDING,
                codes::INVALID_EVENT,
                codes::INVALID_EVENT
            ]
        );
        let mut timer = order(&json!({}));
        timer["transitions"][2]["trigger"] = json!({"kind": "timer", "millis": "0"});
        timer["transitions"][2]["mode"] = json!("request");
        let found: Vec<_> = check_declaration(&lifecycle(timer), &ORDER)
            .into_iter()
            .map(|finding| finding.code)
            .collect();
        assert_eq!(found, vec![codes::INVALID_TRIGGER, codes::INVALID_TRIGGER]);
    }

    #[test]
    fn a_chart_without_endings_is_reported_not_passed() {
        let chart = lifecycle(json!({
            "property": "status", "initial": ["active"],
            "transitions": [
                {"id": "rebuild", "from": ["active"], "to": "building"},
                {"id": "finish", "from": ["building"], "to": "active"}
            ]
        }));
        let states = ["active", "building"];
        // D84: a chart that declares no LIFE-SOUND member keeps its admission output.
        assert!(check_machine(&Machine::declared(&chart, states.to_vec())).is_empty());
        let mut opted = chart;
        opted.events = vec![norn_ir::LifecycleEvent {
            id: "audit".into(),
            ignored_in: vec![norn_ir::LifecycleIgnore {
                states: vec!["active".into(), "building".into()],
                reason: "read only".into(),
            }],
        }];
        let found = check_machine(&Machine::declared(&opted, states.to_vec()));
        assert_eq!(found.len(), 1);
        assert!(found[0].info && found[0].code == codes::NO_ENDING);
        let mut resting = opted;
        resting.endings = vec![norn_ir::LifecycleEnding {
            state: "active".into(),
            outcome: LifecycleOutcome::Resting,
        }];
        assert!(check_machine(&Machine::declared(&resting, states.to_vec())).is_empty());
    }
}
